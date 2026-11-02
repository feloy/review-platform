//! Lead-only draft presentation, publication, rejection, and recovery commands.

use crate::{draft_commands, escape_terminal_controls, storage};
use anyhow::{Context, Result};
use review_platform::{
    PrKey, RefreshOutcome, RejectOutcome, State,
    config::{ConfigError, ReviewConfig},
    github::{
        self, GithubError,
        publisher::{self, GithubPublisher, PostError, PublisherError},
    },
    publication::{
        AttemptOutcome, PublicationHash, PublicationReceipt, PublicationStateError, SealedBody,
    },
    review::{DraftOutput, Freshness, RoundId, RoundRecord, RoundStateError, RoundStatus},
};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RejectError {
    #[error(transparent)]
    Draft(#[from] draft_commands::DraftError),
    #[error(transparent)]
    Round(#[from] RoundStateError),
    #[error("the supplied body hash does not match the sealed publication body")]
    BodyHashMismatch,
    #[error("local rejection: {0}")]
    Local(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl From<anyhow::Error> for RejectError {
    fn from(error: anyhow::Error) -> Self {
        Self::Local(error.into())
    }
}

#[derive(Debug, Error)]
pub enum PublishError {
    #[error(transparent)]
    Draft(#[from] draft_commands::DraftError),
    #[error(transparent)]
    Round(#[from] RoundStateError),
    #[error(transparent)]
    Body(#[from] PublicationStateError),
    #[error("PR is closed or the draft's reviewed head or author discussion is stale")]
    Stale,
    #[error("reading publication configuration: {0}")]
    ReadConfig(#[source] std::io::Error),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Publisher(#[from] PublisherError),
    #[error(transparent)]
    Github(#[from] GithubError),
    #[error("publication outcome unknown; recover-publication is required: {0}")]
    OutcomeUnknown(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("local publication state: {0}")]
    Local(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl From<anyhow::Error> for PublishError {
    fn from(error: anyhow::Error) -> Self {
        Self::Local(error.into())
    }
}

/// A confirmed local receipt, with an optional post-publication freshness check.
pub struct PublicationReport {
    record: RoundRecord,
    freshness: Option<Freshness>,
}

impl PublicationReport {
    pub fn freshness(&self) -> Option<Freshness> {
        self.freshness
    }
}

fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn operator() -> Result<String> {
    #[cfg(unix)]
    {
        Ok(format!("uid:{}", rustix::process::geteuid().as_raw()))
    }
    #[cfg(windows)]
    {
        let mut buffer = [0u16; 257];
        let mut len = buffer.len() as u32;
        // SAFETY: Windows writes at most `len` UTF-16 units into this live buffer.
        let ok = unsafe {
            windows_sys::Win32::System::WindowsProgramming::GetUserNameW(
                buffer.as_mut_ptr(),
                &mut len,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error()).context("reading the Windows account");
        }
        let used = usize::try_from(len)?.saturating_sub(1);
        let name = String::from_utf16(&buffer[..used])?;
        anyhow::ensure!(!name.is_empty(), "operating-system principal is empty");
        Ok(format!("windows-user:{name}"))
    }
}

fn sealed_draft(
    state_path: &Path,
    state: &mut State,
    key: &PrKey,
    id: &RoundId,
) -> Result<(DraftOutput, SealedBody, bool)> {
    let (draft, seal) = draft_commands::read_verified(state_path, state, key, id)?.into_contents();
    let changed = state.seal_round(id, seal.clone())?;
    Ok((draft, seal, changed))
}

pub fn show_draft(
    state_override: Option<PathBuf>,
    repository: String,
    pr: u64,
    round: String,
    show_log: bool,
) -> Result<()> {
    let key = PrKey::new(repository, pr)?;
    let id = RoundId::new(round)?;
    let state_path = storage::resolve_path(state_override)?;
    let _pr_lock = storage::pr_lock(&state_path, &key)?;
    let (record, draft, seal, confirmed_ids) = {
        let _lock = storage::lock(&state_path)?;
        let mut state = storage::load(&state_path)?;
        let (draft, seal, changed) = sealed_draft(&state_path, &mut state, &key, &id)?;
        if changed {
            storage::save(&state_path, &state)?;
        }
        let record = state.round(&id).context("round disappeared")?.clone();
        let confirmed_ids = state.confirmed_review_ids(&key);
        (record, draft, seal, confirmed_ids)
    };
    let freshness = draft_commands::github_freshness(&record, &confirmed_ids);
    println!("Draft {id} for {key}");
    println!("  reviewed head: {}", record.head_sha());
    println!("  input revision: {}", record.revision());
    println!("  publication body SHA-256: {}", seal.hash());
    println!("  decision: {}", record.status());
    if let Some(attempt) = record.current_attempt() {
        println!(
            "  publication attempt: {} ({})",
            attempt.id(),
            attempt.outcome()
        );
    }
    println!("  agent verdict: {}", draft.status());
    println!("  freshness: {freshness}");
    if let Some(receipt) = record.receipt() {
        println!(
            "  published review: {} ({})",
            receipt.review_id(),
            receipt.url()
        );
    }
    if let Some(rejection) = record.rejection() {
        println!(
            "  rejected by: {} at {}",
            rejection.operator(),
            rejection.at()
        );
    }
    println!(
        "\nExact GitHub publication body:\n{}",
        escape_terminal_controls(seal.body())
    );
    if let Err(error) = seal.ensure_github_limit() {
        println!("\nPublication unavailable: {error}");
    } else if record.status() == RoundStatus::AwaitingDecision {
        println!(
            "\nPublish command:\nreview-platform publish-draft {} {} --round {} --body-hash {}",
            key.repository(),
            key.number(),
            id,
            seal.hash()
        );
    }
    if matches!(
        record.status(),
        RoundStatus::Publishing | RoundStatus::PublicationUnknown
    ) {
        println!(
            "\nRecovery command:\nreview-platform recover-publication {} {} --round {}",
            key.repository(),
            key.number(),
            id
        );
    }
    println!(
        "\nPrivate lead advice:\n{}",
        escape_terminal_controls(draft.lead_advice())
    );
    if show_log {
        println!(
            "\nPrivate review log:\n{}",
            escape_terminal_controls(draft.review_log())
        );
    }
    Ok(())
}

pub fn reject_draft(
    state_override: Option<PathBuf>,
    repository: String,
    pr: u64,
    round: String,
    body_hash: String,
) -> Result<()> {
    let key = PrKey::new(repository, pr)?;
    let id = RoundId::new(round)?;
    let hash = PublicationHash::new(body_hash)?;
    let state_path = storage::resolve_path(state_override)?;
    match reject(&state_path, &key, &id, &hash)? {
        RejectOutcome::Rejected => {
            println!("Round {id} for {key} rejected locally; current authorization is preserved.")
        }
        RejectOutcome::AlreadyRejected => {
            println!("Round {id} for {key} was already rejected; authorization is unchanged.")
        }
    }
    Ok(())
}

/// Record the lead's decision for this exact draft, without requiring fresh PR inputs.
pub fn reject(
    state_path: &Path,
    key: &PrKey,
    id: &RoundId,
    hash: &PublicationHash,
) -> Result<RejectOutcome, RejectError> {
    let _pr_lock = storage::pr_lock(state_path, key)?;
    let _lock = storage::lock(state_path)?;
    let mut state = storage::load(state_path)?;
    if let Some(record) = state.round(id) {
        if record.key() == key && record.status() == RoundStatus::Rejected {
            if let Some(seal) = record.sealed_body() {
                if seal.hash() != hash {
                    return Err(RejectError::BodyHashMismatch);
                }
                return Ok(RejectOutcome::AlreadyRejected);
            }
        }
    }
    let (_, seal) = draft_commands::read_verified(state_path, &state, key, id)?.into_contents();
    if seal.hash() != hash {
        return Err(RejectError::BodyHashMismatch);
    }
    let changed = state.seal_round(id, seal)?;
    let outcome = state.reject_round(key, id, hash, operator()?, now()?)?;
    if changed || outcome == RejectOutcome::Rejected {
        storage::save(state_path, &state)?;
    }
    Ok(outcome)
}

fn refreshed_freshness(
    client: &github::GithubClient,
    state_path: &Path,
    round: &RoundRecord,
) -> Result<Freshness, PublishError> {
    let key = round.key();
    // save() atomically replaces the state file, so an unlocked read sees a complete snapshot.
    // Reload under the lock before applying the GitHub observation.
    let state = storage::load(state_path)?;
    let confirmed_ids = state.confirmed_review_ids(key);
    let observation = client.observe_pull_excluding(key, &confirmed_ids)?;
    let pull = client.pull_request(key)?;
    let freshness = round.publication_freshness(
        observation.head_sha(),
        observation.discussion(),
        pull.head_sha(),
        pull.availability(),
    );
    let _lock = storage::lock(state_path)?;
    let mut state = storage::load(state_path)?;
    let outcome = state
        .refresh(key, observation.head_sha(), observation.discussion())
        .map_err(RoundStateError::from)?;
    if outcome != RefreshOutcome::Unchanged {
        storage::save(state_path, &state)?;
    }
    Ok(freshness)
}

fn require_fresh(
    client: &github::GithubClient,
    state_path: &Path,
    round: &RoundRecord,
) -> Result<(), PublishError> {
    match refreshed_freshness(client, state_path, round)? {
        Freshness::Fresh => Ok(()),
        Freshness::Stale | Freshness::Unknown => Err(PublishError::Stale),
    }
}

fn print_receipt(round: &RoundRecord) -> Result<()> {
    let receipt = round.receipt().context("published round has no receipt")?;
    println!(
        "Round {} published as GitHub review {}: {}",
        round.id(),
        receipt.review_id(),
        receipt.url()
    );
    println!(
        "  publisher: {} ({})",
        receipt.publisher_login(),
        receipt.publisher_id()
    );
    println!("  reviewed commit: {}", receipt.commit_id());
    println!("  published at: {}", receipt.published_at());
    Ok(())
}

fn finish_attempt(
    state_path: &Path,
    id: &RoundId,
    attempt_id: u64,
    outcome: AttemptOutcome,
) -> Result<()> {
    let _lock = storage::lock(state_path)?;
    let mut state = storage::load(state_path)?;
    state.finish_publication_attempt(id, attempt_id, outcome, now()?)?;
    storage::save(state_path, &state)
}

pub fn publish_draft(
    state_override: Option<PathBuf>,
    config_override: Option<PathBuf>,
    repository: String,
    pr: u64,
    round: String,
    body_hash: String,
) -> Result<()> {
    let key = PrKey::new(repository, pr)?;
    let id = RoundId::new(round)?;
    let hash = PublicationHash::new(body_hash)?;
    let state_path = storage::resolve_path(state_override)?;
    let config_path = storage::resolve_config_path(config_override)?;
    let report = publish(&state_path, &config_path, &key, &id, &hash).with_context(|| {
        format!("publishing round {id}; if outcome is unknown, run recover-publication {} {} --round {id}", key.repository(), key.number())
    })?;
    print_receipt(&report.record)?;
    match report.freshness() {
        None | Some(Freshness::Fresh) => {}
        Some(Freshness::Stale) => println!(
            "  PR inputs changed after publication; the review remains tied to its original commit."
        ),
        Some(Freshness::Unknown) => {
            println!("  PR freshness could not be rechecked after publication.")
        }
    }
    Ok(())
}

/// Publish only the exact approved seal, keeping uncertain dispatches out of retries.
pub fn publish(
    state_path: &Path,
    config_path: &Path,
    key: &PrKey,
    id: &RoundId,
    hash: &PublicationHash,
) -> Result<PublicationReport, PublishError> {
    let _pr_lock = storage::pr_lock(state_path, key)?;
    let (record, seal) = {
        let _lock = storage::lock(state_path)?;
        let mut state = storage::load(state_path)?;
        if let Some(record) = state.round(id) {
            if record.key() == key && record.status() == RoundStatus::Published {
                let seal = record
                    .sealed_body()
                    .ok_or_else(|| RoundStateError::InvalidRecord(id.clone()))?;
                if seal.hash() != hash {
                    return Err(PublicationStateError::BodyHashMismatch.into());
                }
                return Ok(PublicationReport {
                    record: record.clone(),
                    freshness: None,
                });
            }
        }
        let (_, seal) = draft_commands::read_verified(state_path, &state, key, id)?.into_contents();
        if seal.hash() != hash {
            return Err(PublicationStateError::BodyHashMismatch.into());
        }
        let record = state
            .round(id)
            .ok_or_else(|| RoundStateError::NotFound(id.clone()))?;
        match record.status() {
            RoundStatus::AwaitingDecision => {}
            RoundStatus::Starting
            | RoundStatus::Running
            | RoundStatus::Publishing
            | RoundStatus::PublicationUnknown
            | RoundStatus::Published
            | RoundStatus::Rejected
            | RoundStatus::Failed
            | RoundStatus::Interrupted => {
                return Err(RoundStateError::WrongStatus(id.clone()).into());
            }
        }
        if record.cleanup_pending() {
            return Err(RoundStateError::CleanupPending(id.clone()).into());
        }
        seal.ensure_github_limit()?;
        // Reject locally stale inputs before loading credentials or contacting GitHub.
        let request = state
            .get(key)
            .ok_or_else(|| RoundStateError::NotFound(id.clone()))?;
        request
            .ensure_inspected(record.head_sha(), record.revision())
            .map_err(RoundStateError::from)?;
        if request.discussion() != Some(record.discussion()) {
            return Err(PublishError::Stale);
        }
        let record = record.clone();
        if state.seal_round(id, seal.clone())? {
            storage::save(state_path, &state)?;
        }
        (record, seal)
    };

    let config_bytes = std::fs::read(config_path).map_err(PublishError::ReadConfig)?;
    let config = ReviewConfig::from_json(&config_bytes, config_path)?;
    let publisher = GithubPublisher::new(config.publisher_login())?;
    let identity = publisher.verify_identity()?;
    let reader = github::GithubClient::new()?;
    require_fresh(&reader, state_path, &record)?;
    let preexisting = publisher
        .reviews(key)?
        .into_iter()
        .filter(|review| review.matches(identity.id().get(), record.head_sha(), seal.body()))
        .map(|review| review.nonzero_id())
        .collect();
    require_fresh(&reader, state_path, &record)?;
    let attempt_id = {
        let _lock = storage::lock(state_path)?;
        let mut state = storage::load(state_path)?;
        let attempt_id = state.reserve_publication(
            key,
            id,
            hash,
            operator()?,
            identity.login().to_owned(),
            identity.id(),
            preexisting,
            now()?,
        )?;
        storage::save(state_path, &state)?;
        attempt_id
    };
    {
        let _lock = storage::lock(state_path)?;
        let mut state = storage::load(state_path)?;
        state.mark_publication_dispatched(id, attempt_id, now()?)?;
        storage::save(state_path, &state)?;
    }
    let result = publisher.create_review(key, record.head_sha(), seal.body(), &identity);
    let published = record_post_result(state_path, id, attempt_id, result)?;
    let freshness =
        refreshed_freshness(&reader, state_path, &published).unwrap_or(Freshness::Unknown);
    Ok(PublicationReport {
        record: published,
        freshness: Some(freshness),
    })
}

/// Persist outcomes even when the browser is gone; a failed save after dispatch needs recovery.
fn record_post_result(
    state_path: &Path,
    id: &RoundId,
    attempt_id: u64,
    result: Result<PublicationReceipt, PostError>,
) -> Result<RoundRecord, PublishError> {
    match result {
        Ok(receipt) => {
            let save = || -> Result<RoundRecord> {
                let _lock = storage::lock(state_path)?;
                let mut state = storage::load(state_path)?;
                state.record_publication(id, attempt_id, receipt, now()?)?;
                storage::save(state_path, &state)?;
                Ok(state.round(id).context("round disappeared")?.clone())
            };
            save().map_err(|error| PublishError::OutcomeUnknown(error.into()))
        }
        Err(PostError::BeforeDispatch(error) | PostError::Definitive(error)) => {
            finish_attempt(
                state_path,
                id,
                attempt_id,
                AttemptOutcome::DefinitiveFailure,
            )
            .map_err(|error| PublishError::OutcomeUnknown(error.into()))?;
            Err(PublishError::Publisher(error))
        }
        Err(PostError::Unknown(error)) => {
            finish_attempt(state_path, id, attempt_id, AttemptOutcome::Unknown)
                .map_err(|error| PublishError::OutcomeUnknown(error.into()))?;
            Err(PublishError::OutcomeUnknown(Box::new(error)))
        }
    }
}

pub fn recover_publication(
    state_override: Option<PathBuf>,
    repository: String,
    pr: u64,
    round: String,
    review_id: Option<NonZeroU64>,
    confirm_not_posted: Option<NonZeroU64>,
) -> Result<()> {
    let key = PrKey::new(repository, pr)?;
    let id = RoundId::new(round)?;
    let state_path = storage::resolve_path(state_override)?;
    let _pr_lock = storage::pr_lock(&state_path, &key)?;
    let (record, seal) = {
        let _lock = storage::lock(&state_path)?;
        let mut state = storage::load(&state_path)?;
        if let Some(record) = state.round(&id) {
            if record.key() == &key && record.status() == RoundStatus::Published {
                return print_receipt(record);
            }
        }
        let (_, seal, changed) = sealed_draft(&state_path, &mut state, &key, &id)?;
        if changed {
            storage::save(&state_path, &state)?;
        }
        (state.round(&id).context("round disappeared")?.clone(), seal)
    };
    if record.status() == RoundStatus::Published {
        return print_receipt(&record);
    }
    anyhow::ensure!(
        matches!(
            record.status(),
            RoundStatus::Publishing | RoundStatus::PublicationUnknown
        ),
        "round {id} does not need publication recovery"
    );
    let attempt = record
        .current_attempt()
        .context("publication attempt is missing")?;
    if attempt.outcome() == AttemptOutcome::Reserved {
        anyhow::ensure!(
            review_id.is_none() && confirm_not_posted.is_none(),
            "reserved attempt was never dispatched; recover without a resolution option"
        );
        finish_attempt(
            &state_path,
            &id,
            attempt.id(),
            AttemptOutcome::DefinitiveFailure,
        )?;
        println!(
            "Round {id}: attempt {} was reserved but not dispatched; draft is awaiting a decision.",
            attempt.id()
        );
        return Ok(());
    }
    if record.status() == RoundStatus::Publishing {
        finish_attempt(&state_path, &id, attempt.id(), AttemptOutcome::Unknown)?;
    }
    if let Some(confirmed_attempt) = confirm_not_posted {
        anyhow::ensure!(
            confirmed_attempt.get() == attempt.id(),
            "attempt ID does not match current publication attempt {}",
            attempt.id()
        );
        let _lock = storage::lock(&state_path)?;
        let mut state = storage::load(&state_path)?;
        state.confirm_not_posted(&id, attempt.id(), operator()?, now()?)?;
        storage::save(&state_path, &state)?;
        println!(
            "Round {id}: operator confirmed attempt {} was not posted; draft is awaiting a new decision.",
            attempt.id()
        );
        return Ok(());
    }
    let publisher = GithubPublisher::new(Some(attempt.expected_publisher_login()))?;
    let _identity = publisher.verify_identity_id(attempt.publisher_id())?;
    let reviews = publisher.reviews(&key)?;
    let candidates = publisher::recovery_matches(&reviews, attempt, record.head_sha(), seal.body());
    let selected = match review_id {
        Some(id) => candidates
            .iter()
            .copied()
            .find(|review| review.id() == id.get())
            .with_context(|| format!("review {id} is not a matching new publication"))?,
        None if candidates.len() == 1 => candidates[0],
        None => {
            if candidates.is_empty() {
                println!(
                    "Round {id}: no matching new review found for attempt {}; publication outcome remains unknown.",
                    attempt.id()
                );
            } else {
                let ids = candidates
                    .iter()
                    .map(|review| review.id().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                println!(
                    "Round {id}: multiple matching new reviews ({ids}) for attempt {}; rerun with --review-id ID.",
                    attempt.id()
                );
            }
            return Ok(());
        }
    };
    let selected_id = selected.id();
    let receipt = reviews
        .into_iter()
        .find(|review| review.id() == selected_id)
        .context("matching review disappeared from the collected page")?
        .into_receipt()?;
    let _lock = storage::lock(&state_path)?;
    let mut state = storage::load(&state_path)?;
    state.record_recovered_publication(&id, attempt.id(), receipt, operator()?, now()?)?;
    storage::save(&state_path, &state)?;
    print_receipt(state.round(&id).context("round disappeared")?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_platform::HeadSha;
    use std::fs;

    fn fixture(directory: &Path) -> (PathBuf, PrKey, RoundId, PublicationHash) {
        let path = directory.join("state.json");
        let id = RoundId::new("r-publish").unwrap();
        let key = PrKey::new("owner/repo", 15).unwrap();
        let artifact = serde_json::to_vec(&serde_json::json!({
            "status":"ACCEPT", "comments":"Exact comments", "lead_advice":"Private advice",
        }))
        .unwrap();
        let seal = SealedBody::new(
            "Exact comments",
            id.as_str(),
            PublicationHash::of(&artifact),
        );
        let hash = seal.hash().clone();
        let round_dir = directory.join("rounds/r-publish");
        fs::create_dir_all(round_dir.join("output")).unwrap();
        fs::write(round_dir.join("output/review-output.json"), artifact).unwrap();
        fs::write(round_dir.join("draft.json"), serde_json::to_vec(&serde_json::json!({
            "status":"ACCEPT", "comments":"Exact comments", "lead_advice":"Private advice", "review_log":"Private log",
        })).unwrap()).unwrap();
        let state = serde_json::json!({
            "schema_version":7,
            "requests":[{"key":key, "head_sha":"a".repeat(40), "authorization":"awaiting_lead", "revision":2,
                "discussion":{"author_id":7,"comments":[]}, "model_validation":null}],
            "rounds":[{"id":id,"key":key,"status":"awaiting_decision", "sandbox":"rp-722d7075626c697368",
                "head_sha":"a".repeat(40),"revision":2,"discussion":{"author_id":7,"comments":[]},
                "model":"model-alpha","agent":"claude","gateway":"gateway","workspace":"default","driver":"vm",
                "model_provider":"model-provider","github_provider":"github-ro","prompt_sha256":"a".repeat(64),
                "format_sha256":"b".repeat(64),"archive_sha256":"c".repeat(64),"archive_config_sha256":"d".repeat(64),
                "execution_sha256":"e".repeat(64),"runner_sha256":"f".repeat(64),"created_at":1,"dispatched_at":2,
                "finished_at":3,"freshness":"fresh","failure":null,"incomplete_log":false,"cleanup_pending":false,
                "sealed_body":seal}],
        });
        let state: State = serde_json::from_value(state).unwrap();
        storage::save(&path, &state).unwrap();
        (path, key, id, hash)
    }

    fn dispatch(path: &Path, key: &PrKey, id: &RoundId, hash: &PublicationHash) -> u64 {
        let mut state = storage::load(path).unwrap();
        let attempt = state
            .reserve_publication(
                key,
                id,
                hash,
                "uid:1".into(),
                "publisher".into(),
                NonZeroU64::new(7).unwrap(),
                vec![],
                4,
            )
            .unwrap();
        state.mark_publication_dispatched(id, attempt, 5).unwrap();
        storage::save(path, &state).unwrap();
        attempt
    }

    fn receipt() -> PublicationReceipt {
        PublicationReceipt::new(
            NonZeroU64::new(71).unwrap(),
            "https://github.com/owner/repo/pull/15#pullrequestreview-71".into(),
            "publisher".into(),
            NonZeroU64::new(7).unwrap(),
            HeadSha::new("a".repeat(40)).unwrap(),
            "2026-11-01T18:00:00Z".into(),
        )
    }

    #[test]
    fn publication_gates_local_changes_cleanup_hash_and_artifacts_before_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let (path, key, id, hash) = fixture(directory.path());
        let config = directory.path().join("no-configuration.json");
        let before = fs::read(&path).unwrap();
        assert!(matches!(
            publish(&path, &config, &key, &id, &PublicationHash::of(b"wrong")),
            Err(PublishError::Body(PublicationStateError::BodyHashMismatch))
        ));
        assert_eq!(fs::read(&path).unwrap(), before);
        let mut state: serde_json::Value = serde_json::from_slice(&before).unwrap();
        state["rounds"][0]["cleanup_pending"] = true.into();
        fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(matches!(
            publish(&path, &config, &key, &id, &hash),
            Err(PublishError::Round(RoundStateError::CleanupPending(_)))
        ));
        fs::write(&path, &before).unwrap();
        let mut state = storage::load(&path).unwrap();
        let discussion = state.get(&key).unwrap().discussion().unwrap().clone();
        state
            .refresh(&key, &HeadSha::new("b".repeat(40)).unwrap(), &discussion)
            .unwrap();
        storage::save(&path, &state).unwrap();
        assert!(matches!(
            publish(&path, &config, &key, &id, &hash),
            Err(PublishError::Round(RoundStateError::State(_)))
        ));
        fs::write(&path, &before).unwrap();
        fs::write(
            directory
                .path()
                .join("rounds/r-publish/output/review-output.json"),
            b"{}",
        )
        .unwrap();
        assert!(matches!(
            publish(&path, &config, &key, &id, &hash),
            Err(PublishError::Draft(_))
        ));
        assert_eq!(fs::read(path).unwrap(), before);
    }

    #[test]
    fn confirmed_publication_is_saved_and_repeated_without_credentials_or_artifacts() {
        let directory = tempfile::tempdir().unwrap();
        let (path, key, id, hash) = fixture(directory.path());
        let attempt = dispatch(&path, &key, &id, &hash);
        let record = record_post_result(&path, &id, attempt, Ok(receipt())).unwrap();
        assert_eq!(record.status(), RoundStatus::Published);
        assert_eq!(record.receipt().unwrap().review_id(), 71);
        let before = fs::read(&path).unwrap();
        fs::remove_file(directory.path().join("rounds/r-publish/draft.json")).unwrap();
        let config = directory.path().join("no-configuration.json");
        let repeated = publish(&path, &config, &key, &id, &hash).ok().unwrap();
        assert_eq!(repeated.record.receipt().unwrap().review_id(), 71);
        assert_eq!(repeated.freshness(), None);
        assert!(matches!(
            publish(&path, &config, &key, &id, &PublicationHash::of(b"other")),
            Err(PublishError::Body(PublicationStateError::BodyHashMismatch))
        ));
        assert_eq!(fs::read(path).unwrap(), before);
    }

    #[test]
    fn uncertain_publication_is_recorded_and_never_automatically_retried() {
        let directory = tempfile::tempdir().unwrap();
        let (path, key, id, hash) = fixture(directory.path());
        let attempt = dispatch(&path, &key, &id, &hash);
        let error = PostError::Unknown(PublisherError::InvalidResponse {
            operation: "create review",
            detail: "private adapter detail".into(),
        });
        assert!(matches!(
            record_post_result(&path, &id, attempt, Err(error)),
            Err(PublishError::OutcomeUnknown(_))
        ));
        let state = storage::load(&path).unwrap();
        assert_eq!(
            state.round(&id).unwrap().status(),
            RoundStatus::PublicationUnknown
        );
        assert_eq!(
            state
                .round(&id)
                .unwrap()
                .current_attempt()
                .unwrap()
                .outcome(),
            AttemptOutcome::Unknown
        );
        let before = fs::read(&path).unwrap();
        assert!(matches!(
            publish(&path, &directory.path().join("unused"), &key, &id, &hash),
            Err(PublishError::Round(RoundStateError::WrongStatus(_)))
        ));
        assert_eq!(fs::read(path).unwrap(), before);
    }

    #[test]
    fn definitive_failure_returns_to_decision_but_failed_receipt_save_requires_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let (path, key, id, hash) = fixture(directory.path());
        let attempt = dispatch(&path, &key, &id, &hash);
        assert!(matches!(
            record_post_result(
                &path,
                &id,
                attempt,
                Err(PostError::Definitive(PublisherError::HttpStatus {
                    operation: "create review",
                    status: 403,
                }))
            ),
            Err(PublishError::Publisher(_))
        ));
        assert_eq!(
            storage::load(&path).unwrap().round(&id).unwrap().status(),
            RoundStatus::AwaitingDecision
        );
        let attempt = dispatch(&path, &key, &id, &hash);
        fs::write(&path, b"invalid local state").unwrap();
        assert!(matches!(
            record_post_result(&path, &id, attempt, Ok(receipt())),
            Err(PublishError::OutcomeUnknown(_))
        ));
    }
}
