//! Operator CLI for PR authorization, review rounds, and draft decisions.

mod gui;
mod publication_commands;
mod review_commands;
mod storage;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use review_platform::{
    Authorization, AuthorizationScope, CommentSource, HeadSha, InputRevision, PrKey, RecordOutcome,
    RefreshOutcome, StateError,
    config::{ModelId, ReviewConfig},
    github, round,
};
use std::fs;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    version,
    about = "Local operator prototype for Review Platform",
    long_about = "Record PR requests and lead decisions, prepare and run reviews through an existing OpenShell gateway, and inspect, publish, reject, or recover private drafts."
)]
struct Cli {
    /// Override the per-user state file; keep this private to the lead operator.
    #[arg(long, global = true)]
    state_file: Option<PathBuf>,

    /// Override the platform review configuration file.
    #[arg(long, global = true)]
    config_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the local graphical interface and API over HTTP.
    Gui,
    /// Manually record a PR review request at its current head SHA.
    Request {
        repository: String,
        pr: u64,
        head_sha: String,
    },
    /// Refresh an existing request's head and author discussion from GitHub.
    Refresh { repository: String, pr: u64 },
    /// Authorize the recorded PR head and input revision the lead inspected.
    Authorize {
        repository: String,
        pr: u64,
        #[arg(long, value_enum)]
        scope: Scope,
        /// Head SHA shown by status; must still be the recorded head.
        #[arg(long, value_name = "HEAD_SHA")]
        head: String,
        /// Input revision shown by status; must still be current.
        #[arg(long, value_name = "REVISION")]
        revision: u64,
    },
    /// Withdraw the lead's authorization for a recorded PR.
    Revoke { repository: String, pr: u64 },
    /// Show a recorded PR's current head and authorization.
    Status { repository: String, pr: u64 },
    /// Refresh inputs, check exact-head CI, and propose or validate a review model.
    PrepareReview {
        repository: String,
        pr: u64,
        /// Record the lead's model choice; requires --head and --revision.
        #[arg(long)]
        model: Option<String>,
        /// Head SHA the lead inspected with prepare-review or status.
        #[arg(long, value_name = "HEAD_SHA")]
        head: Option<String>,
        /// Input revision the lead inspected with prepare-review or status.
        #[arg(long, value_name = "REVISION")]
        revision: Option<u64>,
    },
    /// Inspect the configured OpenShell gateway without changing it.
    Gateway {
        #[command(subcommand)]
        command: GatewayCommand,
    },
    /// Run one prepared, authorized review in the foreground.
    RunReview {
        repository: String,
        pr: u64,
        #[arg(long, value_name = "HEAD_SHA")]
        head: String,
        #[arg(long, value_name = "REVISION")]
        revision: u64,
    },
    /// Display a saved private draft and optionally its sensitive review log.
    ShowDraft {
        repository: String,
        pr: u64,
        #[arg(long, value_name = "ROUND_ID")]
        round: String,
        #[arg(long)]
        log: bool,
    },
    /// Reject a private draft locally without posting it.
    RejectDraft {
        repository: String,
        pr: u64,
        #[arg(long, value_name = "ROUND_ID")]
        round: String,
        /// SHA-256 of the exact body displayed by show-draft.
        #[arg(long, value_name = "SHA256")]
        body_hash: String,
    },
    /// Publish the sealed draft as a COMMENT review on its reviewed commit.
    PublishDraft {
        repository: String,
        pr: u64,
        #[arg(long, value_name = "ROUND_ID")]
        round: String,
        #[arg(long, value_name = "SHA256")]
        body_hash: String,
    },
    /// Resolve a reserved or uncertain publication attempt.
    RecoverPublication {
        repository: String,
        pr: u64,
        #[arg(long, value_name = "ROUND_ID")]
        round: String,
        /// Select one matching new GitHub review when recovery finds several.
        #[arg(long, conflicts_with = "confirm_not_posted")]
        review_id: Option<NonZeroU64>,
        /// Record the lead's manual assertion that this attempt did not post.
        #[arg(long, value_name = "ATTEMPT_ID")]
        confirm_not_posted: Option<NonZeroU64>,
    },
    /// Clean up an interrupted review without running the agent again.
    RecoverReview { repository: String, pr: u64 },
    /// List locally recorded review requests.
    List,
    /// Print the absolute path of the config file without reading it.
    ConfigPath,
    /// Show a live GitHub PR, optionally including its code changes.
    GithubPr {
        repository: String,
        pr: u64,
        /// Show check runs and commit statuses for the current PR head.
        #[arg(long)]
        checks: bool,
        /// Show the full PR diff against its base, useful for the first review.
        #[arg(long, conflicts_with = "since")]
        diff: bool,
        /// Show changes after a supplied ancestor head SHA.
        #[arg(long, value_name = "HEAD_SHA")]
        since: Option<String>,
    },
    /// Print the absolute path of the state file without reading it.
    StatePath,
}

#[derive(Subcommand)]
enum GatewayCommand {
    Status,
}

#[derive(Clone, Copy, ValueEnum)]
enum Scope {
    OneRound,
    PrCycle,
}

impl From<Scope> for AuthorizationScope {
    fn from(scope: Scope) -> Self {
        match scope {
            Scope::OneRound => Self::OneRound,
            Scope::PrCycle => Self::PrCycle,
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let Cli {
        state_file,
        config_file,
        command,
    } = cli;
    match command {
        Command::Gui => gui::run(state_file)?,
        Command::Request {
            repository,
            pr,
            head_sha,
        } => {
            let state_file = storage::resolve_path(state_file)?;
            let key = PrKey::new(repository, pr)?;
            let head_sha = HeadSha::new(head_sha)?;
            let _pr_lock = storage::pr_lock(&state_file, &key)?;
            let _lock = storage::lock(&state_file)?;
            let mut state = storage::load(&state_file)?;
            let outcome = state.record_request(key.clone(), head_sha)?;
            if outcome != RecordOutcome::Unchanged {
                storage::save(&state_file, &state)?;
            }
            match outcome {
                RecordOutcome::New => println!("Recorded {key}; awaiting lead authorization."),
                RecordOutcome::Unchanged => println!("{key} already has this head SHA."),
                RecordOutcome::HeadChangedAuthorizationReset => {
                    println!("Updated {key}; one-round authorization was cleared.")
                }
                RecordOutcome::HeadChangedAuthorizationPreserved => {
                    println!("Updated {key}; authorization is unchanged.")
                }
            }
        }
        Command::Refresh { repository, pr } => {
            let state_file = storage::resolve_path(state_file)?;
            let key = PrKey::new(repository, pr)?;
            let _pr_lock = storage::pr_lock(&state_file, &key)?;
            let state = storage::load(&state_file)?;
            let prior_revision = state
                .get(&key)
                .ok_or_else(|| StateError::RequestNotFound(key.clone()))?
                .revision();
            let client = github::GithubClient::new()?;
            let observation =
                client.observe_pull_excluding(&key, &state.confirmed_review_ids(&key))?;
            let _lock = storage::lock(&state_file)?;
            let mut state = storage::load(&state_file)?;
            let current_revision = state
                .get(&key)
                .ok_or_else(|| StateError::RequestNotFound(key.clone()))?
                .revision();
            if current_revision != prior_revision {
                return Err(StateError::RevisionMismatch {
                    inspected: prior_revision,
                    recorded: current_revision,
                }
                .into());
            }
            let outcome = state.refresh(
                observation.key(),
                observation.head_sha(),
                observation.discussion(),
            )?;
            if outcome != RefreshOutcome::Unchanged {
                storage::save(&state_file, &state)?;
            }
            match outcome {
                RefreshOutcome::Unchanged => println!("{key}: inputs unchanged."),
                RefreshOutcome::Updated {
                    head_changed,
                    discussion_changed,
                    authorization_cleared,
                } => {
                    let mut changes = Vec::new();
                    if head_changed {
                        changes.push("head changed");
                    }
                    if discussion_changed {
                        changes.push("author discussion changed or established");
                    }
                    println!("{key}: {}.", changes.join(", "));
                    if authorization_cleared {
                        println!("  one-round authorization was cleared.");
                    }
                }
            }
        }
        Command::Authorize {
            repository,
            pr,
            scope,
            head,
            revision,
        } => {
            let state_file = storage::resolve_path(state_file)?;
            let key = PrKey::new(repository, pr)?;
            let inspected_head = HeadSha::new(head)?;
            let inspected_revision = InputRevision::new(revision)?;
            let scope = AuthorizationScope::from(scope);
            let _lock = storage::lock(&state_file)?;
            let mut state = storage::load(&state_file)?;
            if state.authorize(&key, &inspected_head, inspected_revision, scope)? {
                storage::save(&state_file, &state)?;
            }
            println!("{key}: {}.", Authorization::from(scope));
        }
        Command::Revoke { repository, pr } => {
            let state_file = storage::resolve_path(state_file)?;
            let key = PrKey::new(repository, pr)?;
            let _lock = storage::lock(&state_file)?;
            let mut state = storage::load(&state_file)?;
            if state.revoke(&key)? {
                storage::save(&state_file, &state)?;
            }
            println!("{key}: {}.", Authorization::AwaitingLead);
        }
        Command::Status { repository, pr } => {
            let state_file = storage::resolve_path(state_file)?;
            let key = PrKey::new(repository, pr)?;
            let state = storage::load(&state_file)?;
            let request = state
                .get(&key)
                .ok_or_else(|| StateError::RequestNotFound(key.clone()))?;
            println!("{}", request.key());
            println!("  head: {}", request.head_sha());
            println!("  revision: {}", request.revision());
            match request.discussion() {
                Some(discussion) => println!(
                    "  author discussion: {} issue comments, {} review comments, {} review bodies",
                    discussion.count(CommentSource::IssueComment),
                    discussion.count(CommentSource::ReviewComment),
                    discussion.count(CommentSource::Review)
                ),
                None => println!("  author discussion: not yet observed"),
            }
            println!("  state: {}", request.authorization());
            if let Some(validation) = request.model_validation() {
                println!(
                    "  model choice: {} via {} (recheck configuration with prepare-review)",
                    validation.model(),
                    validation.agent()
                );
            }
            if let Some(round) = state.latest_round(&key) {
                println!("  latest round: {} ({})", round.id(), round.status());
                if round.cleanup_pending() {
                    println!("  cleanup pending: run recover-review");
                }
                if matches!(
                    round.status(),
                    review_platform::review::RoundStatus::Publishing
                        | review_platform::review::RoundStatus::PublicationUnknown
                ) {
                    println!(
                        "  publication recovery: review-platform recover-publication {} {} --round {}",
                        key.repository(),
                        key.number(),
                        round.id()
                    );
                }
            }
        }
        Command::PrepareReview {
            repository,
            pr,
            model,
            head,
            revision,
        } => run_prepare(
            state_file,
            config_file,
            repository,
            pr,
            model,
            head,
            revision,
        )?,
        Command::Gateway {
            command: GatewayCommand::Status,
        } => {
            review_commands::gateway_status(config_file)?;
        }
        Command::RunReview {
            repository,
            pr,
            head,
            revision,
        } => {
            review_commands::run_review(state_file, config_file, repository, pr, head, revision)?;
        }
        Command::ShowDraft {
            repository,
            pr,
            round,
            log,
        } => {
            publication_commands::show_draft(state_file, repository, pr, round, log)?;
        }
        Command::RejectDraft {
            repository,
            pr,
            round,
            body_hash,
        } => {
            publication_commands::reject_draft(state_file, repository, pr, round, body_hash)?;
        }
        Command::PublishDraft {
            repository,
            pr,
            round,
            body_hash,
        } => {
            publication_commands::publish_draft(
                state_file,
                config_file,
                repository,
                pr,
                round,
                body_hash,
            )?;
        }
        Command::RecoverPublication {
            repository,
            pr,
            round,
            review_id,
            confirm_not_posted,
        } => {
            publication_commands::recover_publication(
                state_file,
                repository,
                pr,
                round,
                review_id,
                confirm_not_posted,
            )?;
        }
        Command::RecoverReview { repository, pr } => {
            review_commands::recover_review(state_file, config_file, repository, pr)?;
        }
        Command::List => {
            let state_file = storage::resolve_path(state_file)?;
            let state = storage::load(&state_file)?;
            let mut requests = state.requests().iter().collect::<Vec<_>>();
            requests.sort_by_key(|request| (request.key().repository(), request.key().number()));
            if requests.is_empty() {
                println!("No review requests recorded.");
            }
            for request in requests {
                println!(
                    "{}  {}  {}",
                    request.key(),
                    request.head_sha(),
                    request.authorization()
                );
            }
        }
        Command::GithubPr {
            repository,
            pr,
            checks,
            diff,
            since,
        } => {
            let key = PrKey::new(repository, pr)?;
            let since = since.map(HeadSha::new).transpose()?;
            let client = github::GithubClient::new()?;
            let pull = client.pull_request(&key)?;
            let changes = if diff || since.is_some() {
                Some(client.diff_for_pull(&key, &pull, since.as_ref())?)
            } else {
                None
            };
            let checks = if checks {
                Some(client.checks_for_pull(&key, &pull)?)
            } else {
                None
            };
            println!("GitHub PR {key}");
            println!("  title: {}", escape_terminal_field(pull.title()));
            println!("  author: {}", pull.author());
            println!("  state: {}", pull.state());
            println!("  draft: {}", if pull.draft() { "yes" } else { "no" });
            println!("  head: {}", pull.head_sha());
            println!(
                "  url: https://github.com/{}/pull/{}",
                key.repository(),
                key.number()
            );
            if let Some(report) = checks {
                println!(
                    "  reported checks: {} ({} check runs, {} commit statuses)",
                    report.summary(),
                    report.check_runs().len(),
                    report.commit_statuses().len()
                );
                let mut runs = report.check_runs().iter().collect::<Vec<_>>();
                runs.sort_by(|left, right| left.name.cmp(&right.name));
                for run in runs {
                    println!(
                        "    check run: {} — {}, conclusion: {}",
                        escape_terminal_field(&run.name),
                        run.status.as_str(),
                        run.conclusion
                            .map_or("none", |conclusion| conclusion.as_str())
                    );
                }
                let mut statuses = report.commit_statuses().iter().collect::<Vec<_>>();
                statuses.sort_by(|left, right| left.context.cmp(&right.context));
                for status in statuses {
                    println!(
                        "    commit status: {} — {}",
                        escape_terminal_field(&status.context),
                        status.state.as_str()
                    );
                }
            }
            if let Some(changes) = changes {
                match since {
                    Some(previous_head) => {
                        println!("\nChanges since {previous_head}:")
                    }
                    None => println!("\nFull PR diff against base {}:", pull.base_sha()),
                }
                if changes.is_empty() {
                    println!("(no changes)");
                } else {
                    let safe_changes = escape_diff_for_terminal(&changes);
                    print!("{safe_changes}");
                    if !safe_changes.ends_with('\n') {
                        println!();
                    }
                }
            }
        }
        Command::StatePath => {
            let state_file = storage::resolve_path(state_file)?;
            println!("{}", state_file.display());
        }
        Command::ConfigPath => {
            let config_file = storage::resolve_config_path(config_file)?;
            println!("{}", config_file.display());
        }
    }
    Ok(())
}

fn run_prepare(
    state_override: Option<PathBuf>,
    config_override: Option<PathBuf>,
    repository: String,
    pr: u64,
    model: Option<String>,
    head: Option<String>,
    revision: Option<u64>,
) -> Result<()> {
    let selection = match (model, head, revision) {
        (None, None, None) => None,
        (Some(model), Some(head), Some(revision)) => Some(round::ModelSelection::new(
            ModelId::new(model)?,
            HeadSha::new(head)?,
            InputRevision::new(revision)?,
        )),
        _ => anyhow::bail!("--model, --head, and --revision must be supplied together"),
    };
    let key = PrKey::new(repository, pr)?;
    let state_file = storage::resolve_path(state_override)?;
    let _pr_lock = storage::pr_lock(&state_file, &key)?;
    let config_file = storage::resolve_config_path(config_override)?;
    let config_bytes = fs::read(&config_file)
        .with_context(|| format!("reading review configuration {}", config_file.display()))?;
    let config = ReviewConfig::from_json(&config_bytes)
        .with_context(|| format!("validating review configuration {}", config_file.display()))?;
    let initial = storage::load(&state_file)?;
    let prior_revision = initial
        .get(&key)
        .ok_or_else(|| StateError::RequestNotFound(key.clone()))?
        .revision();
    let client = github::GithubClient::new()?;
    let observation = client.observe_pull_excluding(&key, &initial.confirmed_review_ids(&key))?;
    let _lock = storage::lock(&state_file)?;
    let mut state = storage::load(&state_file)?;
    let current_revision = state
        .get(&key)
        .ok_or_else(|| StateError::RequestNotFound(key.clone()))?
        .revision();
    if current_revision != prior_revision {
        return Err(StateError::RevisionMismatch {
            inspected: prior_revision,
            recorded: current_revision,
        }
        .into());
    }
    let refreshed = round::refresh_observation(&mut state, &config, observation)?;
    if refreshed.outcome() != RefreshOutcome::Unchanged {
        storage::save(&state_file, &state)?;
    }
    drop(_lock);
    let report = round::prepare(&client, &mut state, refreshed, selection.as_ref())?;
    if report.validation_changed() {
        let verified = review_platform::archive::verify_archive(report.resolved().archive())?;
        let _lock = storage::lock(&state_file)?;
        let mut current = storage::load(&state_file)?;
        let selected = selection
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("model selection disappeared"))?;
        current.validate_model(
            report.preparation(),
            selected.head(),
            selected.revision(),
            report.resolved(),
            &verified,
        )?;
        storage::save(&state_file, &current)?;
        state = current;
    }
    let request = state
        .get(&key)
        .ok_or_else(|| StateError::RequestNotFound(key.clone()))?;
    let preparation = report.preparation();
    let resolved = report.resolved();
    println!("{key}");
    println!("  head: {}", request.head_sha());
    println!("  revision: {}", request.revision());
    println!("  authorization: {}", request.authorization());
    println!("  checks: {}", report.checks());
    println!("  proposed model: {}", preparation.proposed_model());
    if preparation.proposal_matches_coauthor() {
        println!("  proposal matches a PR co-author hint; no different model is configured");
    }
    if let Some(validated) = preparation.validated_model() {
        println!("  validated model: {validated}");
    }
    if let Some(requested) = &selection {
        println!("  requested model: {}", requested.model());
    }
    println!("  resolved agent: {}", resolved.agent().id());
    println!(
        "  runner kind: {}",
        resolved.agent().runner().kind().as_str()
    );
    println!(
        "  model provider: {}",
        resolved.agent().runner().model_provider()
    );
    println!("  prompt SHA-256: {}", resolved.agent().prompt_hash());
    println!("  runner SHA-256: {}", resolved.agent().runner().hash());
    println!("  VM archive: {}", resolved.archive().path().display());
    println!("  archive SHA-256: {}", resolved.archive().checksum());
    println!(
        "  VM metadata: {}",
        resolved.archive().config_path().display()
    );
    println!(
        "  metadata SHA-256: {}",
        resolved.archive().config_checksum()
    );
    println!("  readiness: {}", preparation.readiness());
    Ok(())
}

pub(crate) fn escape_terminal_controls(value: &str) -> String {
    let mut rendered = String::with_capacity(value.len());
    for character in value.chars() {
        if character == '\n' || character == '\t' {
            rendered.push(character);
        } else if character.is_control()
            || matches!(
                character,
                '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
            )
        {
            rendered.extend(character.escape_default());
        } else {
            rendered.push(character);
        }
    }
    rendered
}

fn escape_diff_for_terminal(value: &str) -> String {
    escape_terminal_controls(&value.replace("\r\n", "\n"))
}

fn escape_terminal_field(value: &str) -> String {
    escape_terminal_controls(value).replace(['\n', '\t'], " ")
}

#[cfg(test)]
mod tests {
    use super::{escape_diff_for_terminal, escape_terminal_controls, escape_terminal_field};

    #[test]
    fn code_changes_cannot_control_the_terminal() {
        assert_eq!(
            escape_terminal_controls("+text\u{1b}[31m\n+other\u{202e}\tend\r"),
            "+text\\u{1b}[31m\n+other\\u{202e}\tend\\r"
        );
        assert_eq!(
            escape_terminal_field("a\n\u{1b}[31m\tjob"),
            "a \\u{1b}[31m job"
        );
        assert_eq!(
            escape_diff_for_terminal("+first\r\n+second\r\n+stray\rtail\n"),
            "+first\n+second\n+stray\\rtail\n"
        );
    }
}
