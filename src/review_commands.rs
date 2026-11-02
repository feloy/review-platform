//! Shared review execution, sandbox recovery, and private draft presentation.

use crate::{preparation_commands::InspectedRules, storage};
use anyhow::{Context, Result};
use review_platform::{
    Authorization, HeadSha, InputRevision, PrKey, RefreshOutcome, State, StateError,
    agent::{self, AgentRunError},
    archive, checkout,
    config::{PromptHash, ReviewConfig},
    github::{self, PullRequestState},
    openshell::{OpenShell, OpenShellError, SandboxMounts},
    preparation::Readiness,
    review::{
        DraftOutput, FailureKind, Freshness, OUTPUT_MARKER, REVIEW_RULES, RoundId, RoundRecord,
        RoundStatus,
    },
    round::{self, ModelSelection},
    sandbox_layout,
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
use thiserror::Error;

/// The frozen preparation the signed-in lead selected for a GUI run.
pub struct InspectedRun {
    selection: ModelSelection,
    rules: InspectedRules,
}

impl InspectedRun {
    pub fn new(selection: ModelSelection, rules: InspectedRules) -> Self {
        Self { selection, rules }
    }

    pub fn selection(&self) -> &ModelSelection {
        &self.selection
    }

    pub fn rules(&self) -> &InspectedRules {
        &self.rules
    }

    fn check(
        &self,
        state: &State,
        key: &PrKey,
        configuration_sha256: &PromptHash,
    ) -> Result<(), InspectionError> {
        let request = state
            .get(key)
            .ok_or_else(|| StateError::RequestNotFound(key.clone()))?;
        request.ensure_inspected(self.selection.head(), self.selection.revision())?;
        if request.authorization() == Authorization::AwaitingLead
            || request.authorization() != self.rules().authorization()
            || configuration_sha256 != self.rules().configuration_sha256()
        {
            return Err(InspectionError::RulesChanged);
        }
        if !request.model_validation().is_some_and(|validation| {
            validation.model() == self.selection.model()
                && validation.head_sha() == self.selection.head()
                && validation.revision() == self.selection.revision()
        }) {
            return Err(InspectionError::ModelChanged);
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum InspectionError {
    #[error(transparent)]
    Config(#[from] review_platform::config::ConfigError),
    #[error(transparent)]
    State(#[from] StateError),
    #[error("authorization or configuration changed; prepare and inspect the review again")]
    RulesChanged,
    #[error("validated model changed; prepare and inspect the review again")]
    ModelChanged,
}

#[derive(Debug, Error)]
pub enum RunError {
    #[error(transparent)]
    Inspection(InspectionError),
    #[error("review execution: {0}")]
    Execution(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Register cancellation once for the owner of one CLI run or the GUI server.
pub struct Cancellation {
    flag: Arc<AtomicBool>,
    #[cfg(unix)]
    signals: Vec<signal_hook::SigId>,
}

impl Cancellation {
    pub fn new() -> Result<Self> {
        let mut cancellation = Self {
            flag: Arc::new(AtomicBool::new(false)),
            #[cfg(unix)]
            signals: Vec::new(),
        };
        cancellation.register()?;
        Ok(cancellation)
    }

    fn register(&mut self) -> Result<()> {
        #[cfg(unix)]
        for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
            self.signals
                .push(signal_hook::flag::register(signal, self.flag.clone())?);
        }
        Ok(())
    }

    pub fn flag(&self) -> Arc<AtomicBool> {
        self.flag.clone()
    }
}

impl Drop for Cancellation {
    fn drop(&mut self) {
        #[cfg(unix)]
        for signal in self.signals.drain(..) {
            signal_hook::low_level::unregister(signal);
        }
    }
}

fn load_config(override_path: Option<PathBuf>) -> Result<(PathBuf, ReviewConfig)> {
    let path = storage::resolve_config_path(override_path)?;
    let bytes = fs::read(&path)
        .with_context(|| format!("reading review configuration {}", path.display()))?;
    let config = ReviewConfig::from_json(&bytes, &path)
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

fn show_draft_command(key: &PrKey, id: &RoundId) -> String {
    format!(
        "show-draft {} {} --round {id}",
        key.repository(),
        key.number()
    )
}

fn cleanup_pending_context(key: &PrKey, id: &RoundId) -> String {
    format!(
        "draft {id} saved; sandbox cleanup pending. Run recover-review {} {}. Run {}.",
        key.repository(),
        key.number(),
        show_draft_command(key, id)
    )
}

fn cleanup_complete_after_failed_stop_context(key: &PrKey, id: &RoundId) -> String {
    format!(
        "draft {id} saved; sandbox cleanup complete after stop failed. Run {}.",
        show_draft_command(key, id)
    )
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
    let config_path = storage::resolve_config_path(config_override)?;
    let cancellation = Cancellation::new()?;
    execute_review(
        &state_path,
        &config_path,
        RunInputs {
            key: &key,
            head: &inspected_head,
            revision: inspected_revision,
            inspection: None,
        },
        cancellation.flag(),
        &mut |_| {},
    )
}

pub fn run_inspected_review(
    state_path: &Path,
    config_path: &Path,
    key: &PrKey,
    inspected: &InspectedRun,
    cancelled: Arc<AtomicBool>,
    started: &mut dyn FnMut(&RoundId),
) -> Result<(), RunError> {
    execute_review(
        state_path,
        config_path,
        RunInputs {
            key,
            head: inspected.selection().head(),
            revision: inspected.selection().revision(),
            inspection: Some(inspected),
        },
        cancelled,
        started,
    )
    .map_err(|error| match error.downcast::<InspectionError>() {
        Ok(error) => RunError::Inspection(error),
        Err(error) => RunError::Execution(error.into()),
    })
}

struct RunInputs<'a> {
    key: &'a PrKey,
    head: &'a HeadSha,
    revision: InputRevision,
    inspection: Option<&'a InspectedRun>,
}

impl RunInputs<'_> {
    fn check(&self, state: &State, configuration_sha256: &PromptHash) -> Result<()> {
        if let Some(inspection) = self.inspection {
            inspection.check(state, self.key, configuration_sha256)?;
        }
        Ok(())
    }
}

fn execute_review(
    state_path: &Path,
    config_path: &Path,
    inputs: RunInputs<'_>,
    cancelled: Arc<AtomicBool>,
    started: &mut dyn FnMut(&RoundId),
) -> Result<()> {
    let RunInputs {
        key,
        head: inspected_head,
        revision: inspected_revision,
        ..
    } = inputs;
    anyhow::ensure!(!cancelled.load(Ordering::Relaxed), "review interrupted");
    let pr_lock = if inputs.inspection.is_some() {
        storage::try_pr_lock(state_path, key)?
    } else {
        storage::pr_lock(state_path, key)?
    };
    let (_, config) = load_config(Some(config_path.to_owned()))?;
    let shell = OpenShell::new(config.execution());
    {
        let _lock = storage::lock(state_path)?;
        let state = storage::load(state_path)?;
        inputs.check(&state, config.hash())?;
        let request = state
            .get(key)
            .with_context(|| format!("review request not found: {key}"))?;
        request.ensure_inspected(inspected_head, inspected_revision)?;
        anyhow::ensure!(
            request.authorization() != Authorization::AwaitingLead,
            "{key} is awaiting lead authorization"
        );
        if let Some(blocking) = state
            .rounds()
            .iter()
            .find(|round| round.key() == key && round.blocks_new_round())
        {
            anyhow::bail!("round {} blocks another review of {key}", blocking.id());
        }
    }
    // A missing or unsupported gateway cannot consume authorization.
    shell.preflight_gateway()?;
    let client = github::GithubClient::new()?;
    let observation = client.observe_pull_excluding(key, &state_confirmed_ids(state_path, key)?)?;
    let mut state = {
        let _lock = storage::lock(state_path)?;
        let mut state = storage::load(state_path)?;
        inputs.check(&state, load_config(Some(config_path.to_owned()))?.1.hash())?;
        state
            .get(key)
            .with_context(|| format!("review request not found: {key}"))?
            .ensure_inspected(inspected_head, inspected_revision)?;
        let refreshed = round::refresh_observation(&mut state, &config, observation)?;
        if refreshed.outcome() != RefreshOutcome::Unchanged {
            storage::save(state_path, &state)?;
        }
        state
            .get(key)
            .with_context(|| format!("review request not found: {key}"))?
            .ensure_inspected(inspected_head, inspected_revision)?;
        state
    };
    let refreshed = round::refresh(&client, &mut state, &config, key)?;
    persist_changed_observation(state_path, &refreshed, inspected_revision)?;
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
    let pull = client.pull_request(key)?;
    anyhow::ensure!(
        pull.head_sha() == inspected_head && pull.state() == PullRequestState::Open,
        "PR head changed or PR closed during preparation"
    );
    let round_dir = new_round_dir(state_path)?;
    let _runner_lock = storage::try_lock(&round_dir.join("runner"))?;
    let id = RoundId::new(
        round_dir
            .file_name()
            .context("round path has no ID")?
            .to_string_lossy()
            .into_owned(),
    )?;
    let discussion = state
        .get(key)
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
    {
        let _lock = storage::lock(state_path)?;
        let mut current = storage::load(state_path)?;
        inputs.check(
            &current,
            load_config(Some(config_path.to_owned()))?.1.hash(),
        )?;
        anyhow::ensure!(!cancelled.load(Ordering::Relaxed), "review interrupted");
        current.begin_round(record.clone(), choice)?;
        storage::save(state_path, &current)?;
    }
    // The saved Starting round blocks another round; the runner lock protects
    // this sandbox until execution and cleanup finish.
    drop(pr_lock);
    started(&id);
    println!("Round {id}: preparing checkout and sandbox.");
    let mut failure = FailureKind::Preflight;
    let mut sandbox_created = false;
    let mut agent_log = String::new();
    let mut incomplete_log = false;
    let operation = (|| -> Result<(
        Freshness,
        Option<OpenShellError>,
        review_platform::publication::SealedBody,
    )> {
        let staged_image = archive::stage_archive(
            choice.archive(),
            &private_parent(state_path).join("archives"),
        )?;
        let executable = staged_image.claude_executable()?;
        if cancelled.load(Ordering::Relaxed) {
            failure = FailureKind::Recovery;
            anyhow::bail!("review interrupted");
        }
        failure = FailureKind::Checkout;
        let checkout_path = round_dir.join("src");
        checkout::create_checkout(&checkout_path, key, inspected_head, pull.base_sha())?;
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
            &instructions.join("review-rules-v1.txt"),
            REVIEW_RULES.as_bytes(),
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
        let live = client.observe_pull_excluding(key, &state_confirmed_ids(state_path, key)?)?;
        let mut current = {
            let _lock = storage::lock(state_path)?;
            let mut current = storage::load(state_path)?;
            current
                .get(key)
                .with_context(|| format!("review request not found: {key}"))?
                .ensure_inspected(record.head_sha(), record.revision())?;
            let refreshed = round::refresh_observation(&mut current, &config, live)?;
            if refreshed.outcome() != RefreshOutcome::Unchanged {
                storage::save(state_path, &current)?;
            }
            anyhow::ensure!(
                current
                    .get(key)
                    .is_some_and(|request| request.head_sha() == record.head_sha()
                        && request.discussion() == Some(record.discussion())),
                "PR head or author discussion changed before dispatch"
            );
            current
        };
        let refreshed = round::refresh(&client, &mut current, &config, key)?;
        persist_changed_observation(state_path, &refreshed, record.revision())?;
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
        let live_pull = client.pull_request(key)?;
        anyhow::ensure!(
            live_pull.head_sha() == record.head_sha()
                && live_pull.base_sha() == pull.base_sha()
                && live_pull.state() == PullRequestState::Open,
            "PR head, base, or open state changed before dispatch"
        );
        let fresh_config = load_config(Some(config_path.to_owned()))?.1;
        let current_choice = fresh_config
            .review_models_for(key)?
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
            let _lock = storage::lock(state_path)?;
            let mut current = storage::load(state_path)?;
            inputs.check(&current, load_config(Some(config_path.to_owned()))?.1.hash())?;
            current.dispatch_round(&id, now()?)?;
            storage::save(state_path, &current)?;
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
        let draft = review_platform::review::read_saved_draft(&round_dir.join("draft.json"))?;
        let (agent, artifact_hash) = review_platform::review::read_agent_output_with_hash(
            &output.join(sandbox_layout::OUTPUT_FILE_NAME),
            String::new(),
        )?;
        anyhow::ensure!(
            draft.status() == agent.status()
                && draft.comments() == agent.comments()
                && draft.lead_advice() == agent.lead_advice(),
            "saved draft differs from the original agent artifact"
        );
        let seal = review_platform::publication::SealedBody::new(
            draft.comments(),
            id.as_str(),
            artifact_hash,
        );
        Ok((refresh_freshness(&client, state_path, &record), stop_error, seal))
    })();
    let (freshness, stop_error, seal) = match operation {
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
            let _lock = storage::lock(state_path)?;
            let mut current = storage::load(state_path)?;
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
            storage::save(state_path, &current)?;
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
        let _lock = storage::lock(state_path)?;
        let mut current = storage::load(state_path)?;
        current.complete_round(&id, now()?, freshness)?;
        current.seal_round(&id, seal)?;
        storage::save(state_path, &current)?;
    }
    finish_completed_cleanup(&shell, &record, state_path, stop_error)?;
    println!(
        "Round {id}: private draft available ({freshness}). Run {}.",
        show_draft_command(key, &id)
    );
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
    let _pr_lock = storage::try_pr_lock(&state_path, &key)?;
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
    let _runner_lock =
        storage::try_recovery_lock(&round_path(&state_path, record.id()).join("runner"))?;
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
    let Ok(observation) = client.observe_pull_excluding(
        round.key(),
        &state_confirmed_ids(state_path, round.key()).unwrap_or_default(),
    ) else {
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

fn state_confirmed_ids(state_path: &Path, key: &PrKey) -> Result<Vec<u64>> {
    // save() atomically replaces the state file, so this unlocked read sees a complete snapshot.
    // Callers recheck freshness or state under the lock before using the observation.
    Ok(storage::load(state_path)?.confirmed_review_ids(key))
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

pub(crate) fn round_path(state_path: &Path, id: &RoundId) -> PathBuf {
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

fn finish_completed_cleanup(
    shell: &OpenShell<'_>,
    record: &RoundRecord,
    state_path: &Path,
    stop_error: Option<OpenShellError>,
) -> Result<()> {
    let key = record.key();
    let id = record.id();
    if let Err(delete_error) = shell.delete(record) {
        return Err(delete_error).with_context(|| {
            let stop_context = stop_error
                .as_ref()
                .map(|error| format!("sandbox stop also failed: {error}; "))
                .unwrap_or_default();
            format!("{stop_context}{}", cleanup_pending_context(key, id))
        });
    }
    {
        let _lock = storage::lock(state_path)?;
        let mut current = storage::load(state_path)?;
        current.mark_round_cleaned(id)?;
        storage::save(state_path, &current)?;
    }
    if let Some(error) = stop_error {
        return Err(error).with_context(|| cleanup_complete_after_failed_stop_context(key, id));
    }
    Ok(())
}

fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inspected_run(configuration_sha256: &PromptHash) -> InspectedRun {
        InspectedRun::new(
            ModelSelection::new(
                review_platform::config::ModelId::new("model-alpha").unwrap(),
                HeadSha::new("a".repeat(40)).unwrap(),
                InputRevision::new(2).unwrap(),
            ),
            serde_json::from_value(serde_json::json!({
                "authorization": "one_round",
                "configuration_sha256": configuration_sha256,
            }))
            .unwrap(),
        )
    }

    #[test]
    fn gui_run_rechecks_frozen_inputs_rules_and_model_before_any_external_work() {
        let dir = tempfile::tempdir().unwrap();
        let state_file = dir.path().join("state.json");
        let config_file = dir.path().join("config.json");
        let config = serde_json::to_vec(&serde_json::json!({
            "schema_version":6,
            "execution":{"openshell":dir.path().join("missing-openshell"), "gateway":"gateway", "workspace":"default", "driver":"vm", "github_provider":"github-ro"},
            "models":[{"id":"model-alpha", "coauthors":[]}],
            "agents":[{"id":"claude", "models":["model-alpha"], "prompt_file":"review.md", "runner":{
                "kind":"claude", "model_provider":"model-provider", "policy":dir.path().join("policy.yaml"), "policy_sha256":"b".repeat(64), "hold_command":["/bin/sleep","3600"]
            }}],
            "repositories":[],
        })).unwrap();
        fs::write(dir.path().join("review.md"), "Original prompt.\n").unwrap();
        fs::write(&config_file, &config).unwrap();
        let key = PrKey::new("owner/repo", 15).unwrap();
        let fixture = serde_json::json!({
            "schema_version":6, "rounds":[],
            "requests":[{
                "key":key, "head_sha":"a".repeat(40), "revision":2, "authorization":"one_round",
                "discussion":{"author_id":7, "comments":[]},
                "model_validation":{
                    "model":"model-alpha", "agent":"claude", "head_sha":"a".repeat(40), "revision":2,
                    "prompt_sha256":"a".repeat(64), "format_sha256":"b".repeat(64),
                    "execution_sha256":"c".repeat(64), "runner_sha256":"d".repeat(64),
                    "vm_archive":{"path":dir.path().join("reviewer.tar"), "sha256":"e".repeat(64), "config_sha256":"f".repeat(64)}
                }
            }],
        });
        let state: State = serde_json::from_value(fixture.clone()).unwrap();
        state.validate().unwrap();
        let configuration_sha256 = load_config(Some(config_file.clone()))
            .unwrap()
            .1
            .hash()
            .clone();
        let inspected = inspected_run(&configuration_sha256);
        inspected
            .check(&state, &key, &configuration_sha256)
            .unwrap();
        // These changes invalidate the same displayed model even when its name is unchanged.
        for field in [
            "head_sha",
            "revision",
            "authorization",
            "model_validation",
            "configuration",
            "prompt_file",
            "selected_model",
            "missing_request",
        ] {
            let mut saved = fixture.clone();
            let mut decision = inspected_run(&configuration_sha256);
            let mut current_config = config.clone();
            fs::write(dir.path().join("review.md"), "Original prompt.\n").unwrap();
            match field {
                "head_sha" => {
                    saved["requests"][0]["head_sha"] = "b".repeat(40).into();
                    saved["requests"][0]["model_validation"] = serde_json::Value::Null;
                }
                "revision" => {
                    saved["requests"][0]["revision"] = 3.into();
                    saved["requests"][0]["model_validation"] = serde_json::Value::Null;
                }
                "authorization" => {
                    saved["requests"][0]["authorization"] = "pr_cycle".into();
                    saved["requests"][0]["model_validation"] = serde_json::Value::Null;
                }
                "model_validation" => {
                    saved["requests"][0]["model_validation"] = serde_json::Value::Null
                }
                "configuration" => current_config.push(b'\n'),
                "prompt_file" => {
                    fs::write(dir.path().join("review.md"), "Changed prompt.\n").unwrap();
                }
                "selected_model" => {
                    decision.selection = ModelSelection::new(
                        review_platform::config::ModelId::new("model-beta").unwrap(),
                        HeadSha::new("a".repeat(40)).unwrap(),
                        InputRevision::new(2).unwrap(),
                    )
                }
                "missing_request" => saved["requests"] = serde_json::json!([]),
                _ => unreachable!(),
            }
            fs::write(&config_file, &current_config).unwrap();
            fs::write(&state_file, serde_json::to_vec(&saved).unwrap()).unwrap();
            let original = fs::read(&state_file).unwrap();
            let error = run_inspected_review(
                &state_file,
                &config_file,
                &key,
                &decision,
                Arc::new(AtomicBool::new(false)),
                &mut |_| panic!("stale run started a round"),
            )
            .unwrap_err();
            let RunError::Inspection(error) = error else {
                panic!("reached external work for {field}: {error}");
            };
            match field {
                "head_sha" => assert!(matches!(
                    error,
                    InspectionError::State(StateError::HeadMismatch { .. })
                )),
                "revision" => assert!(matches!(
                    error,
                    InspectionError::State(StateError::RevisionMismatch { .. })
                )),
                "missing_request" => assert!(matches!(
                    error,
                    InspectionError::State(StateError::RequestNotFound(_))
                )),
                "configuration" | "prompt_file" | "authorization" => {
                    assert!(matches!(error, InspectionError::RulesChanged))
                }
                "model_validation" | "selected_model" => {
                    assert!(matches!(error, InspectionError::ModelChanged))
                }
                _ => unreachable!(),
            }
            assert_eq!(fs::read(&state_file).unwrap(), original);
            assert!(!dir.path().join("rounds").exists());
        }
    }

    #[test]
    fn cleanup_error_includes_copyable_recovery_and_draft_commands() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let id = RoundId::new("r-draft").unwrap();
        assert_eq!(
            cleanup_pending_context(&key, &id),
            "draft r-draft saved; sandbox cleanup pending. Run recover-review owner/repo 42. Run show-draft owner/repo 42 --round r-draft."
        );
        assert_eq!(
            cleanup_complete_after_failed_stop_context(&key, &id),
            "draft r-draft saved; sandbox cleanup complete after stop failed. Run show-draft owner/repo 42 --round r-draft."
        );
    }

    #[test]
    fn recovery_refuses_promptly_while_a_pr_operation_is_active() {
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let key = PrKey::new("owner/repo", 42).unwrap();
        let pr_lock = storage::pr_lock(&state_path, &key).unwrap();
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = recover_review(Some(state_path), None, "owner/repo".to_owned(), 42)
                .map_err(|error| error.to_string());
            sender.send(result).unwrap();
        });
        let blocked = receiver.recv_timeout(Duration::from_secs(3));
        drop(pr_lock);
        worker.join().unwrap();
        assert_eq!(
            blocked
                .expect("recovery waited for the PR lock")
                .unwrap_err(),
            "another operation on this PR is active"
        );
    }

    #[test]
    fn recovery_refuses_while_a_runner_is_active() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let config_path = dir.path().join("config.json");
        let id = RoundId::new("r-test").unwrap();
        let config = serde_json::json!({
            "schema_version": 6,
            "execution": {
                "openshell": dir.path().join("openshell"),
                "gateway": "test-gateway",
                "workspace": "default",
                "driver": "vm",
                "github_provider": "test-github-readonly"
            },
            "models": [],
            "agents": [],
            "repositories": []
        });
        fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
        let state = serde_json::json!({
            "schema_version": 6,
            "requests": [{
                "key": {"repository": "owner/repo", "number": 42},
                "head_sha": "a".repeat(40),
                "authorization": "pr_cycle",
                "revision": 2,
                "discussion": {"author_id": 7, "comments": []},
                "model_validation": null
            }],
            "rounds": [{
                "id": "r-test",
                "key": {"repository": "owner/repo", "number": 42},
                "status": "running",
                "sandbox": "rp-722d74657374",
                "head_sha": "a".repeat(40),
                "revision": 2,
                "discussion": {"author_id": 7, "comments": []},
                "model": "model-alpha",
                "agent": "claude",
                "gateway": "test-gateway",
                "workspace": "default",
                "driver": "vm",
                "model_provider": "test-model-provider",
                "github_provider": "test-github-readonly",
                "prompt_sha256": "a".repeat(64),
                "format_sha256": "b".repeat(64),
                "archive_sha256": "c".repeat(64),
                "archive_config_sha256": "d".repeat(64),
                "execution_sha256": "e".repeat(64),
                "runner_sha256": "f".repeat(64),
                "created_at": 1,
                "dispatched_at": 2,
                "finished_at": null,
                "freshness": null,
                "failure": null,
                "incomplete_log": false,
                "cleanup_pending": false
            }]
        });
        fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
        storage::load(&state_path).unwrap();

        let _runner_lock = storage::try_lock(&round_path(&state_path, &id).join("runner")).unwrap();
        let error = recover_review(Some(state_path), Some(config_path), "owner/repo".into(), 42)
            .unwrap_err();
        assert_eq!(error.to_string(), "review runner is still active");
    }

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

    #[cfg(unix)]
    #[test]
    fn completed_round_attempts_delete_even_when_stop_fails() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let executable = dir.path().join("openshell");
        let calls = dir.path().join("calls.txt");
        let fail_delete = dir.path().join("fail-delete");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \" $* \" in\n  *\" sandbox stop \"*) exit 1;;\n  *\" sandbox delete \"*) if [ -e '{}' ]; then exit 1; fi;;\nesac\nexit 0\n",
            calls.display(),
            fail_delete.display()
        );
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let config = ReviewConfig::from_json(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 6,
                "execution": {
                    "openshell": executable,
                    "gateway": "test-gateway",
                    "workspace": "default",
                    "driver": "vm",
                    "github_provider": "test-github-readonly"
                },
                "models": [],
                "agents": [],
                "repositories": []
            }))
            .unwrap(),
            &dir.path().join("config.json"),
        )
        .unwrap();
        let state = serde_json::json!({
            "schema_version": 5,
            "requests": [{
                "key": {"repository": "owner/repo", "number": 42},
                "head_sha": "a".repeat(40),
                "authorization": "awaiting_lead",
                "revision": 2,
                "discussion": {"author_id": 7, "comments": []},
                "model_validation": null
            }],
            "rounds": [{
                "id": "r-draft",
                "key": {"repository": "owner/repo", "number": 42},
                "status": "draft_available",
                "sandbox": "rp-722d6472616674",
                "head_sha": "a".repeat(40),
                "revision": 2,
                "discussion": {"author_id": 7, "comments": []},
                "model": "model-alpha",
                "agent": "claude",
                "gateway": "test-gateway",
                "workspace": "default",
                "driver": "vm",
                "model_provider": "test-model-provider",
                "github_provider": "test-github-readonly",
                "prompt_sha256": "a".repeat(64),
                "format_sha256": "b".repeat(64),
                "archive_sha256": "c".repeat(64),
                "archive_config_sha256": "d".repeat(64),
                "execution_sha256": "e".repeat(64),
                "runner_sha256": "f".repeat(64),
                "created_at": 1,
                "dispatched_at": 2,
                "finished_at": 3,
                "freshness": "fresh",
                "failure": null,
                "incomplete_log": false,
                "cleanup_pending": true
            }]
        });
        fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
        let id = RoundId::new("r-draft").unwrap();
        let record = storage::load(&state_path)
            .unwrap()
            .round(&id)
            .unwrap()
            .clone();
        let shell = OpenShell::new(config.execution());

        fs::write(&fail_delete, b"").unwrap();
        let stop_error = shell.stop(&record).unwrap_err();
        let error =
            finish_completed_cleanup(&shell, &record, &state_path, Some(stop_error)).unwrap_err();
        assert!(error.to_string().contains("sandbox cleanup pending"));
        assert!(error.to_string().contains("sandbox stop also failed"));
        assert!(
            storage::load(&state_path)
                .unwrap()
                .round(&id)
                .unwrap()
                .cleanup_pending()
        );

        fs::remove_file(&fail_delete).unwrap();
        let stop_error = shell.stop(&record).unwrap_err();
        let error =
            finish_completed_cleanup(&shell, &record, &state_path, Some(stop_error)).unwrap_err();
        assert!(error.to_string().contains("sandbox cleanup complete"));
        assert!(
            !storage::load(&state_path)
                .unwrap()
                .round(&id)
                .unwrap()
                .cleanup_pending()
        );
        assert_eq!(
            fs::read_to_string(calls)
                .unwrap()
                .matches("sandbox delete")
                .count(),
            2
        );
    }
}
