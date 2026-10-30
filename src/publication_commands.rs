//! Lead-only draft presentation, publication, rejection, and recovery commands.

use crate::{escape_terminal_controls, storage};
use anyhow::{Context, Result};
use review_platform::{
    PrKey, RefreshOutcome, RejectOutcome, State,
    github::{
        self,
        publisher::{self, GithubPublisher, PostError},
    },
    publication::{AttemptOutcome, PublicationHash, SealedBody},
    review::{self, DraftOutput, Freshness, RoundId, RoundRecord, RoundStatus},
};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn round_path(state_path: &Path, id: &RoundId) -> PathBuf {
    state_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("rounds")
        .join(id.as_str())
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
    let record = state
        .round(id)
        .with_context(|| format!("round {id} does not exist"))?;
    anyhow::ensure!(record.key() == key, "round {id} belongs to another PR");
    anyhow::ensure!(
        record.status().has_saved_draft(),
        "round {id} has no saved draft"
    );
    let base = round_path(state_path, id);
    let draft = review::read_saved_draft(&base.join("draft.json"))?;
    let (agent, artifact_hash) = review::read_agent_output_with_hash(
        &base.join("output").join("review-output.json"),
        String::new(),
    )?;
    anyhow::ensure!(
        draft.status() == agent.status()
            && draft.comments() == agent.comments()
            && draft.lead_advice() == agent.lead_advice(),
        "saved draft differs from the original agent artifact"
    );
    let seal = SealedBody::new(draft.comments(), id.as_str(), artifact_hash);
    let changed = state.seal_round(id, seal.clone())?;
    Ok((draft, seal, changed))
}

fn checked_hash(seal: &SealedBody, supplied: &PublicationHash) -> Result<()> {
    anyhow::ensure!(
        seal.hash() == supplied,
        "the supplied body hash does not match the sealed publication body"
    );
    Ok(())
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
    let freshness = github::GithubClient::new()
        .and_then(|client| {
            let observation = client.observe_pull_excluding(&key, &confirmed_ids)?;
            let pull = client.pull_request(&key)?;
            Ok((observation, pull))
        })
        .map(|(observation, pull)| {
            record.publication_freshness(
                observation.head_sha(),
                observation.discussion(),
                pull.head_sha(),
                pull.availability(),
            )
        })
        .unwrap_or(Freshness::Unknown);
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
    } else {
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
    let _pr_lock = storage::pr_lock(&state_path, &key)?;
    let _lock = storage::lock(&state_path)?;
    let mut state = storage::load(&state_path)?;
    if let Some(record) = state.round(&id) {
        if record.key() == &key && record.status() == RoundStatus::Rejected {
            if let Some(seal) = record.sealed_body() {
                checked_hash(seal, &hash)?;
                println!("Round {id} for {key} was already rejected; authorization is unchanged.");
                return Ok(());
            }
        }
    }
    let (_, seal, changed) = sealed_draft(&state_path, &mut state, &key, &id)?;
    checked_hash(&seal, &hash)?;
    let outcome = state.reject_round(&key, &id, &hash, operator()?, now()?)?;
    if changed || outcome == RejectOutcome::Rejected {
        storage::save(&state_path, &state)?;
    }
    match outcome {
        RejectOutcome::Rejected => {
            println!("Round {id} for {key} rejected locally; current authorization is preserved.")
        }
        RejectOutcome::AlreadyRejected => {
            println!("Round {id} for {key} was already rejected; authorization is unchanged.")
        }
    }
    Ok(())
}

fn refreshed_freshness(
    client: &github::GithubClient,
    state_path: &Path,
    round: &RoundRecord,
) -> Result<Freshness> {
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
    let outcome = state.refresh(key, observation.head_sha(), observation.discussion())?;
    if outcome != RefreshOutcome::Unchanged {
        storage::save(state_path, &state)?;
    }
    Ok(freshness)
}

fn require_fresh(
    client: &github::GithubClient,
    state_path: &Path,
    round: &RoundRecord,
) -> Result<()> {
    anyhow::ensure!(
        refreshed_freshness(client, state_path, round)? == Freshness::Fresh,
        "PR is closed or the draft's reviewed head or author discussion is stale"
    );
    Ok(())
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
    let _pr_lock = storage::pr_lock(&state_path, &key)?;
    let (record, seal) = {
        let _lock = storage::lock(&state_path)?;
        let mut state = storage::load(&state_path)?;
        if let Some(record) = state.round(&id) {
            if record.key() == &key && record.status() == RoundStatus::Published {
                let seal = record
                    .sealed_body()
                    .context("published round has no sealed body")?;
                checked_hash(seal, &hash)?;
                return print_receipt(record);
            }
        }
        let (_, seal, changed) = sealed_draft(&state_path, &mut state, &key, &id)?;
        checked_hash(&seal, &hash)?;
        if changed {
            storage::save(&state_path, &state)?;
        }
        (state.round(&id).context("round disappeared")?.clone(), seal)
    };
    if record.status() == RoundStatus::Published {
        return print_receipt(&record);
    }
    anyhow::ensure!(
        record.status() == RoundStatus::AwaitingDecision,
        "round {id} is not awaiting a decision; recover uncertain publication first"
    );
    anyhow::ensure!(
        !record.cleanup_pending(),
        "round {id} still needs sandbox cleanup"
    );
    seal.ensure_github_limit()?;

    let config_path = storage::resolve_config_path(config_override)?;
    let config_bytes = std::fs::read(&config_path)
        .with_context(|| format!("reading {}", config_path.display()))?;
    let config = review_platform::config::ReviewConfig::from_json(&config_bytes)?;
    let publisher = GithubPublisher::new(config.publisher_login())?;
    let identity = publisher.verify_identity()?;
    let reader = github::GithubClient::new()?;
    require_fresh(&reader, &state_path, &record)?;
    let preexisting = publisher
        .reviews(&key)?
        .into_iter()
        .filter(|review| review.matches(identity.id().get(), record.head_sha(), seal.body()))
        .map(|review| review.nonzero_id())
        .collect();
    require_fresh(&reader, &state_path, &record)?;
    let attempt_id = {
        let _lock = storage::lock(&state_path)?;
        let mut state = storage::load(&state_path)?;
        let attempt_id = state.reserve_publication(
            &key,
            &id,
            &hash,
            operator()?,
            identity.login().to_owned(),
            identity.id(),
            preexisting,
            now()?,
        )?;
        storage::save(&state_path, &state)?;
        attempt_id
    };
    {
        let _lock = storage::lock(&state_path)?;
        let mut state = storage::load(&state_path)?;
        state.mark_publication_dispatched(&id, attempt_id, now()?)?;
        storage::save(&state_path, &state)?;
    }
    match publisher.create_review(&key, record.head_sha(), seal.body(), &identity) {
        Ok(receipt) => {
            let published = {
                let _lock = storage::lock(&state_path)?;
                let mut state = storage::load(&state_path)?;
                state.record_publication(&id, attempt_id, receipt, now()?)?;
                storage::save(&state_path, &state)?;
                state.round(&id).context("round disappeared")?.clone()
            };
            print_receipt(&published)?;
            match refreshed_freshness(&reader, &state_path, &published) {
                Ok(Freshness::Fresh) => {}
                Ok(Freshness::Stale) => println!(
                    "  PR inputs changed after publication; the review remains tied to its original commit."
                ),
                Ok(Freshness::Unknown) | Err(_) => {
                    println!("  PR freshness could not be rechecked after publication.")
                }
            }
            Ok(())
        }
        Err(PostError::BeforeDispatch(error) | PostError::Definitive(error)) => {
            finish_attempt(
                &state_path,
                &id,
                attempt_id,
                AttemptOutcome::DefinitiveFailure,
            )?;
            Err(error.into())
        }
        Err(PostError::Unknown(error)) => {
            finish_attempt(&state_path, &id, attempt_id, AttemptOutcome::Unknown)?;
            Err(error).context(format!(
                "publication outcome unknown; run recover-publication {} {} --round {id}",
                key.repository(),
                key.number()
            ))
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
