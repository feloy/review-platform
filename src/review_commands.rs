//! Foreground review CLI orchestration and private draft presentation.

use crate::{escape_terminal_controls, storage};
use anyhow::{Context, Result};
use review_platform::{
    Authorization, HeadSha, InputRevision, PrKey, RefreshOutcome, RejectOutcome,
    agent::{self, AgentRunError},
    archive, checkout,
    config::ReviewConfig,
    github::{self, PullRequestState},
    openshell::{OpenShell, OpenShellError, SandboxMounts},
    preparation::Readiness,
    review::{
        DraftOutput, FORMAT_RULES, FailureKind, Freshness, OUTPUT_MARKER, RoundId, RoundRecord,
        RoundStatus,
    },
    round, sandbox_layout,
};
#[cfg(unix)]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::{Builder, NamedTempFile};

fn load_config(override_path: Option<PathBuf>) -> Result<(PathBuf, ReviewConfig)> {
    let path = storage::resolve_config_path(override_path)?;
    let bytes = fs::read(&path)
        .with_context(|| format!("reading review configuration {}", path.display()))?;
    let config = ReviewConfig::from_json(&bytes)
        .with_context(|| format!("validating review configuration {}", path.display()))?;
    Ok((path, config))
}

pub fn gateway_status(config_override: Option<PathBuf>) -> Result<()> {
    let (_, config) = load_config(config_override)?;
    let execution = config.execution();
    let shell = OpenShell::new(execution);
    shell.preflight_gateway()?;
    println!(
        "Gateway {} in workspace {}: authenticated and available.",
        execution.gateway(),
        execution.workspace()
    );
    println!("  driver: vm");
    println!("  GitHub provider: {}", execution.github_provider());
    for agent in config.agents() {
        shell.preflight_runner(agent.runner())?;
        println!(
            "  agent {} ({}): model provider {} and policy available",
            agent.id(),
            agent.runner().kind().as_str(),
            agent.runner().model_provider()
        );
    }
    Ok(())
}

pub fn run_review(
    state_override: Option<PathBuf>,
    config_override: Option<PathBuf>,
    repository: String,
    pr: u64,
    head: String,
    revision: u64,
) -> Result<()> {
    let key = PrKey::new(repository, pr)?;
    let inspected_head = HeadSha::new(head)?;
    let inspected_revision = InputRevision::new(revision)?;
    let state_path = storage::resolve_path(state_override)?;
    let (config_path, config) = load_config(config_override)?;
    let shell = OpenShell::new(config.execution());
    {
        let _lock = storage::lock(&state_path)?;
        let state = storage::load(&state_path)?;
        let request = state
            .get(&key)
            .with_context(|| format!("review request not found: {key}"))?;
        request.ensure_inspected(&inspected_head, inspected_revision)?;
        anyhow::ensure!(
            request.authorization() != Authorization::AwaitingLead,
            "{key} is awaiting lead authorization"
        );
        if let Some(blocking) = state
            .rounds()
            .iter()
            .find(|round| round.key() == &key && round.blocks_new_round())
        {
            anyhow::bail!("round {} blocks another review of {key}", blocking.id());
        }
    }
    // A missing or unsupported gateway cannot consume authorization.
    shell.preflight_gateway()?;
    let client = github::GithubClient::new()?;
    let observation = client.observe_pull(&key)?;
    let mut state = {
        let _lock = storage::lock(&state_path)?;
        let mut state = storage::load(&state_path)?;
        state
            .get(&key)
            .with_context(|| format!("review request not found: {key}"))?
            .ensure_inspected(&inspected_head, inspected_revision)?;
        let refreshed = round::refresh_observation(&mut state, &config, observation)?;
        if refreshed.outcome() != RefreshOutcome::Unchanged {
            storage::save(&state_path, &state)?;
        }
        state
            .get(&key)
            .with_context(|| format!("review request not found: {key}"))?
            .ensure_inspected(&inspected_head, inspected_revision)?;
        state
    };
    let refreshed = round::refresh(&client, &mut state, &config, &key)?;
    persist_changed_observation(&state_path, &refreshed, inspected_revision)?;
    anyhow::ensure!(
        refreshed.outcome() == RefreshOutcome::Unchanged,
        "PR inputs changed during preparation; refresh and inspect the new revision"
    );
    let report = round::prepare(&client, &mut state, refreshed, None)?;
    anyhow::ensure!(
        report.preparation().readiness() == Readiness::Ready,
        "review is not ready: {}",
        report.preparation().readiness()
    );
    let choice = report.resolved();
    shell.preflight_runner(choice.agent().runner())?;
    let pull = client.pull_request(&key)?;
    anyhow::ensure!(
        pull.head_sha() == &inspected_head && pull.state() == PullRequestState::Open,
        "PR head changed or PR closed during preparation"
    );
    let round_dir = new_round_dir(&state_path)?;
    let _runner_lock = storage::try_lock(&round_dir.join("runner"))?;
    let id = RoundId::new(
        round_dir
            .file_name()
            .context("round path has no ID")?
            .to_string_lossy()
            .into_owned(),
    )?;
    let discussion = state
        .get(&key)
        .and_then(|request| request.discussion())
        .context("author discussion is unavailable")?
        .clone();
    let record = RoundRecord::new(
        id.clone(),
        key.clone(),
        inspected_head.clone(),
        inspected_revision,
        discussion,
        choice,
        now()?,
    );
    let cancelled = cancellation_flag()?;
    {
        let _lock = storage::lock(&state_path)?;
        let mut current = storage::load(&state_path)?;
        current.begin_round(record.clone(), choice)?;
        storage::save(&state_path, &current)?;
    }
    println!("Round {id}: preparing checkout and sandbox.");
    let mut failure = FailureKind::Preflight;
    let mut sandbox_created = false;
    let mut agent_log = String::new();
    let mut incomplete_log = false;
    let operation = (|| -> Result<(Freshness, Option<OpenShellError>)> {
        let staged_image = archive::stage_archive(
            choice.archive(),
            &private_parent(&state_path).join("archives"),
        )?;
        let executable = staged_image.claude_executable()?;
        if cancelled.load(Ordering::Relaxed) {
            failure = FailureKind::Recovery;
            anyhow::bail!("review interrupted");
        }
        failure = FailureKind::Checkout;
        let checkout_path = round_dir.join("src");
        checkout::create_checkout(&checkout_path, &key, &inspected_head, pull.base_sha())?;
        if cancelled.load(Ordering::Relaxed) {
            failure = FailureKind::Recovery;
            anyhow::bail!("review interrupted");
        }
        let instructions = round_dir.join("instructions");
        let output = round_dir.join("output");
        fs::create_dir(&instructions).context("creating private instruction directory")?;
        fs::create_dir(&output).context("creating private output directory")?;
        write_new(
            &output.join(sandbox_layout::OUTPUT_FILE_NAME),
            OUTPUT_MARKER.as_bytes(),
        )?;
        write_new(
            &instructions.join("configured-prompt.txt"),
            choice.agent().prompt().as_bytes(),
        )?;
        write_new(
            &instructions.join("output-format-v1.txt"),
            FORMAT_RULES.as_bytes(),
        )?;
        let composed = agent::compose_system_prompt(choice.agent().prompt());
        write_new(&instructions.join("system-prompt.txt"), composed.as_bytes())?;
        failure = FailureKind::SandboxCreate;
        let private_policy = round_dir.join("sandbox-policy.yaml");
        shell.stage_policy(choice.agent().runner(), &private_policy)?;
        // Creation can succeed remotely even if the CLI loses its response.
        sandbox_created = true;
        shell.create(
            &record,
            choice.agent().runner(),
            &staged_image,
            &private_policy,
            &SandboxMounts::new(&checkout_path, &instructions, &output),
        )?;
        shell.check_agent_executable(&record, executable)?;
        if cancelled.load(Ordering::Relaxed) {
            failure = FailureKind::Recovery;
            anyhow::bail!("review interrupted");
        }
        println!("Round {id}: rechecking PR inputs and authorization.");
        failure = FailureKind::Preflight;
        let live = client.observe_pull(&key)?;
        let mut current = {
            let _lock = storage::lock(&state_path)?;
            let mut current = storage::load(&state_path)?;
            current
                .get(&key)
                .with_context(|| format!("review request not found: {key}"))?
                .ensure_inspected(record.head_sha(), record.revision())?;
            let refreshed = round::refresh_observation(&mut current, &config, live)?;
            if refreshed.outcome() != RefreshOutcome::Unchanged {
                storage::save(&state_path, &current)?;
            }
            anyhow::ensure!(
                current
                    .get(&key)
                    .is_some_and(|request| request.head_sha() == record.head_sha()
                        && request.discussion() == Some(record.discussion())),
                "PR head or author discussion changed before dispatch"
            );
            current
        };
        let refreshed = round::refresh(&client, &mut current, &config, &key)?;
        persist_changed_observation(&state_path, &refreshed, record.revision())?;
        anyhow::ensure!(
            refreshed.outcome() == RefreshOutcome::Unchanged,
            "PR inputs changed before dispatch"
        );
        let readiness = round::prepare(&client, &mut current, refreshed, None)?;
        anyhow::ensure!(
            readiness.preparation().readiness() == Readiness::Ready,
            "review is not ready: {}",
            readiness.preparation().readiness()
        );
        let live_pull = client.pull_request(&key)?;
        anyhow::ensure!(
            live_pull.head_sha() == record.head_sha()
                && live_pull.base_sha() == pull.base_sha()
                && live_pull.state() == PullRequestState::Open,
            "PR head, base, or open state changed before dispatch"
        );
        let fresh_config = load_config(Some(config_path.clone()))?.1;
        let current_choice = fresh_config
            .review_models_for(&key)?
            .into_iter()
            .find(|candidate| candidate.model().id() == record.model())
            .context("validated model is no longer configured")?;
        anyhow::ensure!(
            current_choice.agent().id() == record.agent()
                && current_choice.agent().prompt_hash() == record.prompt_hash()
                && current_choice.agent().runner().hash() == record.runner_hash()
                && current_choice.execution().hash() == record.execution_hash()
                && current_choice.archive() == choice.archive(),
            "review configuration changed after model validation; run prepare-review again"
        );
        shell.verify_policy(current_choice.agent().runner())?;
        if cancelled.load(Ordering::Relaxed) {
            failure = FailureKind::Recovery;
            anyhow::bail!("review interrupted");
        }
        failure = FailureKind::Dispatch;
        {
            let _lock = storage::lock(&state_path)?;
            let mut current = storage::load(&state_path)?;
            current.dispatch_round(&id, now()?)?;
            storage::save(&state_path, &current)?;
        }
        println!("Round {id}: running Claude.");
        let prompt_path = sandbox_layout::system_prompt_file();
        let result_path = sandbox_layout::review_output_file();
        let run = agent::run_claude(
            &shell,
            config.execution(),
            choice.agent().runner(),
            &agent::ClaudeInvocation::new(
                executable,
                &record,
                &prompt_path,
                pull.base_sha().as_str(),
                &result_path,
            ),
            cancelled.clone(),
        );
        agent_log = run.log;
        if let Err(error) = run.result {
            failure = match error {
                AgentRunError::Timeout => FailureKind::Timeout,
                AgentRunError::Interrupted => FailureKind::Recovery,
                _ => FailureKind::Agent,
            };
            incomplete_log = true;
            anyhow::bail!("{error}");
        }
        let stop_result = shell.stop(&record);
        println!("Round {id}: validating private draft.");
        failure = FailureKind::Output;
        let stop_error = save_output_after_stop_attempt(
            stop_result,
            &output.join(sandbox_layout::OUTPUT_FILE_NAME),
            &round_dir.join("draft.json"),
            agent_log.clone(),
        )?;
        Ok((refresh_freshness(&client, &state_path, &record), stop_error))
    })();
    let (freshness, stop_error) = match operation {
        Ok(result) => result,
        Err(error) => {
            let cleaned = if sandbox_created {
                let _ = shell.stop(&record);
                shell.delete(&record).is_ok()
            } else {
                true
            };
            if !agent_log.is_empty() {
                let _ = write_new(
                    &round_dir.join(if incomplete_log {
                        "review-log-incomplete.txt"
                    } else {
                        "review-log.txt"
                    }),
                    agent_log.as_bytes(),
                );
            }
            let _lock = storage::lock(&state_path)?;
            let mut current = storage::load(&state_path)?;
            current.fail_round(
                &id,
                now()?,
                failure,
                failure == FailureKind::Recovery,
                incomplete_log,
            )?;
            if cleaned {
                current.mark_round_cleaned(&id)?;
            }
            storage::save(&state_path, &current)?;
            return Err(error).with_context(|| {
                format!(
                    "round {id} failed{}",
                    if cleaned {
                        ""
                    } else {
                        "; run recover-review to clean its sandbox"
                    }
                )
            });
        }
    };
    {
        let _lock = storage::lock(&state_path)?;
        let mut current = storage::load(&state_path)?;
        current.complete_round(&id, now()?, freshness)?;
        storage::save(&state_path, &current)?;
    }
    if let Some(error) = stop_error {
        return Err(error).with_context(|| {
            format!("draft {id} saved; sandbox cleanup pending; run recover-review")
        });
    }
    shell.delete(&record).with_context(|| {
        format!("draft {id} saved; sandbox cleanup pending; run recover-review")
    })?;
    {
        let _lock = storage::lock(&state_path)?;
        let mut current = storage::load(&state_path)?;
        current.mark_round_cleaned(&id)?;
        storage::save(&state_path, &current)?;
    }
    println!(
        "Round {id}: private draft available ({freshness}). Run show-draft {} {} --round {id}.",
        key.repository(),
        key.number()
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
    let state = storage::load(&state_path)?;
    let record = state
        .round(&id)
        .with_context(|| format!("round {id} does not exist"))?;
    anyhow::ensure!(record.key() == &key, "round {id} belongs to another PR");
    anyhow::ensure!(
        record.status().has_saved_draft(),
        "round {id} has no available draft"
    );
    let path = round_path(&state_path, &id).join("draft.json");
    let draft = review_platform::review::read_saved_draft(&path)?;
    let freshness = github::GithubClient::new()
        .and_then(|client| client.observe_pull(&key))
        .map(|observation| freshness_for(record, &observation))
        .unwrap_or(Freshness::Unknown);
    println!("Draft {id} for {key}");
    println!("  round state: {}", record.status());
    println!("  status: {}", draft.status());
    println!("  freshness: {freshness}");
    println!(
        "\nPR-facing comments:\n{}",
        escape_terminal_controls(draft.comments())
    );
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
) -> Result<()> {
    let key = PrKey::new(repository, pr)?;
    let id = RoundId::new(round)?;
    let state_path = storage::resolve_path(state_override)?;
    let _lock = storage::lock(&state_path)?;
    let mut state = storage::load(&state_path)?;
    match state.reject_round(&key, &id)? {
        RejectOutcome::Rejected => {
            storage::save(&state_path, &state)?;
            println!("Round {id} for {key} rejected locally; fresh authorization is required.");
        }
        RejectOutcome::AlreadyRejected => {
            println!("Round {id} for {key} was already rejected; authorization is unchanged.");
        }
    }
    Ok(())
}

pub fn recover_review(
    state_override: Option<PathBuf>,
    config_override: Option<PathBuf>,
    repository: String,
    pr: u64,
) -> Result<()> {
    let key = PrKey::new(repository, pr)?;
    let state_path = storage::resolve_path(state_override)?;
    let (_, config) = load_config(config_override)?;
    let record = {
        let _lock = storage::lock(&state_path)?;
        let state = storage::load(&state_path)?;
        state
            .rounds()
            .iter()
            .rev()
            .find(|round| round.key() == &key && needs_recovery(round))
            .cloned()
            .with_context(|| format!("no interrupted round needs recovery for {key}"))?
    };
    let _runner_lock = storage::try_lock(&round_path(&state_path, record.id()).join("runner"))?;
    {
        let _lock = storage::lock(&state_path)?;
        let state = storage::load(&state_path)?;
        let current = state
            .round(record.id())
            .context("round disappeared during recovery")?;
        anyhow::ensure!(
            needs_recovery(current),
            "round {} no longer needs recovery",
            record.id()
        );
    }
    let shell = OpenShell::new(config.execution());
    let _ = shell.stop(&record);
    shell.delete(&record)?;
    {
        let _lock = storage::lock(&state_path)?;
        let mut state = storage::load(&state_path)?;
        let current = state
            .round(record.id())
            .context("round disappeared during recovery")?;
        if matches!(
            current.status(),
            RoundStatus::Starting | RoundStatus::Running
        ) {
            state.fail_round(
                record.id(),
                now()?,
                FailureKind::Recovery,
                true,
                current.status() == RoundStatus::Running,
            )?;
        }
        state.mark_round_cleaned(record.id())?;
        storage::save(&state_path, &state)?;
    }
    println!(
        "Round {}: sandbox cleanup complete; agent execution was not repeated.",
        record.id()
    );
    Ok(())
}

fn needs_recovery(round: &RoundRecord) -> bool {
    round.cleanup_pending()
        || matches!(round.status(), RoundStatus::Starting | RoundStatus::Running)
}

fn refresh_freshness(
    client: &github::GithubClient,
    state_path: &Path,
    round: &RoundRecord,
) -> Freshness {
    let baseline_revision = match storage::load(state_path)
        .ok()
        .and_then(|state| state.get(round.key()).map(|request| request.revision()))
    {
        Some(revision) => revision,
        None => return Freshness::Unknown,
    };
    let Ok(observation) = client.observe_pull(round.key()) else {
        return Freshness::Unknown;
    };
    let freshness = freshness_for(round, &observation);
    let saved = (|| -> Result<()> {
        let _lock = storage::lock(state_path)?;
        let mut state = storage::load(state_path)?;
        let current_revision = state
            .get(round.key())
            .context("review request disappeared")?
            .revision();
        anyhow::ensure!(
            current_revision == baseline_revision,
            "inputs were refreshed concurrently"
        );
        let outcome = state.refresh(
            observation.key(),
            observation.head_sha(),
            observation.discussion(),
        )?;
        if outcome != RefreshOutcome::Unchanged {
            storage::save(state_path, &state)?;
        }
        Ok(())
    })();
    if saved.is_err() {
        Freshness::Unknown
    } else {
        freshness
    }
}

fn persist_changed_observation(
    state_path: &Path,
    refreshed: &round::RefreshedRound<'_>,
    expected_revision: InputRevision,
) -> Result<()> {
    if refreshed.outcome() == RefreshOutcome::Unchanged {
        return Ok(());
    }
    let observation = refreshed.observation();
    let _lock = storage::lock(state_path)?;
    let mut state = storage::load(state_path)?;
    let current_revision = state
        .get(observation.key())
        .context("review request disappeared")?
        .revision();
    anyhow::ensure!(
        current_revision == expected_revision,
        "inputs were refreshed concurrently"
    );
    let outcome = state.refresh(
        observation.key(),
        observation.head_sha(),
        observation.discussion(),
    )?;
    if outcome != RefreshOutcome::Unchanged {
        storage::save(state_path, &state)?;
    }
    Ok(())
}

fn freshness_for(
    round: &RoundRecord,
    observation: &github::discussion::PullObservation,
) -> Freshness {
    if observation.head_sha() == round.head_sha() && observation.discussion() == round.discussion()
    {
        Freshness::Fresh
    } else {
        Freshness::Stale
    }
}

fn cancellation_flag() -> Result<Arc<AtomicBool>> {
    let flag = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    {
        signal_hook::flag::register(signal_hook::consts::SIGINT, flag.clone())?;
        signal_hook::flag::register(signal_hook::consts::SIGTERM, flag.clone())?;
    }
    Ok(flag)
}

fn private_parent(state_path: &Path) -> PathBuf {
    state_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

fn new_round_dir(state_path: &Path) -> Result<PathBuf> {
    let root = private_parent(state_path).join("rounds");
    fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    }
    let directory = Builder::new().prefix("r-").tempdir_in(&root)?;
    Ok(directory.keep())
}

fn round_path(state_path: &Path, id: &RoundId) -> PathBuf {
    private_parent(state_path).join("rounds").join(id.as_str())
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("creating private file {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn save_draft(path: &Path, draft: &DraftOutput) -> Result<()> {
    let parent = path.parent().context("draft path has no parent")?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut temporary, draft)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(path)?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn save_output_after_stop_attempt(
    stop_result: Result<(), OpenShellError>,
    output_path: &Path,
    draft_path: &Path,
    agent_log: String,
) -> Result<Option<OpenShellError>> {
    let draft = review_platform::review::read_agent_output(output_path, agent_log)?;
    save_draft(draft_path, &draft)?;
    Ok(stop_result.err())
}

fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_output_is_saved_after_stop_failure() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("review-output.json");
        let draft = dir.path().join("draft.json");
        fs::write(
            &output,
            br#"{"status":"REQUEST_CHANGES","comments":"Saved finding","lead_advice":"Private advice"}"#,
        )
        .unwrap();

        let stop_error = save_output_after_stop_attempt(
            Err(OpenShellError::Failed("sandbox stop")),
            &output,
            &draft,
            "Review log".to_owned(),
        )
        .unwrap();
        assert!(matches!(
            stop_error,
            Some(OpenShellError::Failed("sandbox stop"))
        ));
        let saved = review_platform::review::read_saved_draft(&draft).unwrap();
        assert_eq!(saved.comments(), "Saved finding");
        assert_eq!(saved.lead_advice(), "Private advice");
        assert_eq!(saved.review_log(), "Review log");
    }
}
