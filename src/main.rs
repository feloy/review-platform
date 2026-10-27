//! Operator CLI for local authorization and read-only GitHub PR inspection.

mod storage;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use review_platform::{
    Authorization, AuthorizationScope, CommentSource, HeadSha, InputRevision, PrKey, RecordOutcome,
    RefreshOutcome, StateError, github,
};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    version,
    about = "Local operator prototype for Review Platform",
    long_about = "Record PR review requests and lead authorization locally, and inspect live GitHub pull requests. CI and OpenShell are not connected yet."
)]
struct Cli {
    /// Override the per-user state file; keep this private to the lead operator.
    #[arg(long, global = true)]
    state_file: Option<PathBuf>,

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
    /// List locally recorded review requests.
    List,
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
    }
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
