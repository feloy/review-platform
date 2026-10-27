//! Operator CLI for local authorization, PR inspection, and round preparation.

mod storage;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use review_platform::{
    Authorization, AuthorizationScope, CommentSource, HeadSha, InputRevision, PrKey, RecordOutcome,
    RefreshOutcome, StateError, archive,
    config::{ConfigError, ModelId, ReviewConfig},
    github::{self, GithubError},
    preparation,
};
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    version,
    about = "Local operator prototype for Review Platform",
    long_about = "Record PR review requests and lead authorization locally, inspect live GitHub pull requests, and prepare review rounds. OpenShell execution is not connected yet."
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
        Command::Request {
            repository,
            pr,
            head_sha,
        } => {
            let state_file = storage::resolve_path(state_file)?;
            let key = PrKey::new(repository, pr)?;
            let head_sha = HeadSha::new(head_sha)?;
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
            let mut state = storage::load(&state_file)?;
            if state.get(&key).is_none() {
                return Err(StateError::RequestNotFound(key).into());
            }
            let client = github::GithubClient::new()?;
            let observation = client.observe_pull(&key)?;
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
            let mut state = storage::load(&state_file)?;
            if state.authorize(&key, &inspected_head, inspected_revision, scope)? {
                storage::save(&state_file, &state)?;
            }
            println!("{key}: {}.", Authorization::from(scope));
        }
        Command::Revoke { repository, pr } => {
            let state_file = storage::resolve_path(state_file)?;
            let key = PrKey::new(repository, pr)?;
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
        (Some(model), Some(head), Some(revision)) => Some((
            ModelId::new(model)?,
            HeadSha::new(head)?,
            InputRevision::new(revision)?,
        )),
        _ => anyhow::bail!("--model, --head, and --revision must be supplied together"),
    };
    let key = PrKey::new(repository, pr)?;
    let state_file = storage::resolve_path(state_override)?;
    let config_file = storage::resolve_config_path(config_override)?;
    let config_bytes = fs::read(&config_file)
        .with_context(|| format!("reading review configuration {}", config_file.display()))?;
    let config = ReviewConfig::from_json(&config_bytes)
        .with_context(|| format!("validating review configuration {}", config_file.display()))?;
    let review_models = config.review_models_for(&key)?;
    let mut state = storage::load(&state_file)?;
    if state.get(&key).is_none() {
        return Err(StateError::RequestNotFound(key).into());
    }
    let client = github::GithubClient::new()?;
    let observation = client.observe_pull(&key)?;
    let outcome = state.refresh(
        observation.key(),
        observation.head_sha(),
        observation.discussion(),
    )?;
    if outcome != RefreshOutcome::Unchanged {
        storage::save(&state_file, &state)?;
    }
    if let Some((_, inspected_head, inspected_revision)) = &selection {
        state
            .get(&key)
            .ok_or_else(|| StateError::RequestNotFound(key.clone()))?
            .ensure_inspected(inspected_head, *inspected_revision)?;
    }
    let pull = client.pull_request(&key)?;
    if pull.head_sha() != observation.head_sha() {
        return Err(GithubError::HeadChanged {
            key,
            expected: observation.head_sha().clone(),
            actual: pull.head_sha().clone(),
        }
        .into());
    }
    if pull.author_id() != observation.author_id() {
        return Err(GithubError::AuthorChanged { key }.into());
    }
    let checks = client.checks_for_pull(&key, &pull)?;
    let coauthors = client.coauthors_for_pull(&key, &pull)?;
    let final_observation = client.observe_pull(&key)?;
    if final_observation.head_sha() != observation.head_sha()
        || final_observation.discussion() != observation.discussion()
    {
        return Err(GithubError::ObservationChanged.into());
    }
    let final_pull = client.pull_request(&key)?;
    if final_pull.head_sha() != pull.head_sha() {
        return Err(GithubError::HeadChanged {
            key,
            expected: pull.head_sha().clone(),
            actual: final_pull.head_sha().clone(),
        }
        .into());
    }
    if final_pull.author_id() != pull.author_id() {
        return Err(GithubError::AuthorChanged { key }.into());
    }
    let pull_observation = final_pull.preparation_observation();
    let ci_observation = checks.preparation_observation();
    let request = state
        .get(&key)
        .ok_or_else(|| StateError::RequestNotFound(key.clone()))?;
    let mut preparation = preparation::prepare(
        request,
        pull_observation,
        ci_observation,
        &review_models,
        coauthors.identities(),
    )?;
    let mut verified_archive = None;
    if let Some((model_id, inspected_head, inspected_revision)) = &selection {
        let chosen = review_models
            .iter()
            .find(|candidate| candidate.model().id() == model_id)
            .ok_or_else(|| ConfigError::ModelNotConfigured {
                repository: key.repository().to_owned(),
                model: model_id.clone(),
            })?;
        if preparation.readiness().permits_validation() {
            let verified = archive::verify_archive(chosen.archive())?;
            if state.validate_model(
                &preparation,
                inspected_head,
                *inspected_revision,
                chosen,
                &verified,
            )? {
                storage::save(&state_file, &state)?;
            }
            verified_archive = Some(verified);
            let request = state
                .get(&key)
                .ok_or_else(|| StateError::RequestNotFound(key.clone()))?;
            preparation = preparation::prepare(
                request,
                pull_observation,
                ci_observation,
                &review_models,
                coauthors.identities(),
            )?;
        }
    }
    let request = state
        .get(&key)
        .ok_or_else(|| StateError::RequestNotFound(key.clone()))?;
    let resolved = selection
        .as_ref()
        .map(|(model, _, _)| model)
        .or_else(|| preparation.validated_model())
        .and_then(|id| {
            review_models
                .iter()
                .find(|candidate| candidate.model().id() == id)
        })
        .or_else(|| {
            review_models
                .iter()
                .find(|candidate| candidate.model().id() == preparation.proposed_model())
        })
        .context("configuration contains no review model")?;
    if preparation.readiness().permits_validation()
        && !verified_archive
            .as_ref()
            .is_some_and(|verified| verified.matches(resolved.archive()))
    {
        let _verified = archive::verify_archive(resolved.archive())?;
    }
    println!("{key}");
    println!("  head: {}", request.head_sha());
    println!("  revision: {}", request.revision());
    println!("  authorization: {}", request.authorization());
    println!("  checks: {}", checks.summary());
    println!("  proposed model: {}", preparation.proposed_model());
    if preparation.proposal_matches_coauthor() {
        println!("  proposal matches a PR co-author hint; no different model is configured");
    }
    if let Some(validated) = preparation.validated_model() {
        println!("  validated model: {validated}");
    }
    if let Some((requested, _, _)) = &selection {
        println!("  requested model: {requested}");
    }
    println!("  resolved agent: {}", resolved.agent().id());
    println!("  prompt SHA-256: {}", resolved.agent().prompt_hash());
    println!("  VM archive: {}", resolved.archive().path().display());
    println!("  archive SHA-256: {}", resolved.archive().checksum());
    println!("  readiness: {}", preparation.readiness());
    Ok(())
}

fn escape_terminal_controls(value: &str) -> String {
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
