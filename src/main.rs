//! Local operator CLI for reading and changing persisted review authorization.

mod storage;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use review_platform::{Authorization, PrKey, RecordOutcome, StateError};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    version,
    about = "Local operator prototype for Review Platform",
    long_about = "Record PR review requests and lead authorization locally. GitHub, CI, and OpenShell are not connected yet."
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
    /// Set the lead's authorization for a recorded PR.
    Authorize {
        repository: String,
        pr: u64,
        #[arg(long, value_enum)]
        scope: Scope,
    },
    /// Withdraw the lead's authorization for a recorded PR.
    Revoke { repository: String, pr: u64 },
    /// Show a recorded PR's current head and authorization.
    Status { repository: String, pr: u64 },
    /// List locally recorded review requests.
    List,
    /// Print the absolute path of the state file without reading it.
    StatePath,
}

#[derive(Clone, Copy, ValueEnum)]
enum Scope {
    OneRound,
    PrCycle,
}

impl From<Scope> for Authorization {
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
    let state_file = storage::resolve_path(cli.state_file)?;
    match cli.command {
        Command::Request {
            repository,
            pr,
            head_sha,
        } => {
            let mut state = storage::load(&state_file)?;
            let key = PrKey::new(repository, pr)?;
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
        Command::Authorize {
            repository,
            pr,
            scope,
        } => {
            let mut state = storage::load(&state_file)?;
            let key = PrKey::new(repository, pr)?;
            let authorization = Authorization::from(scope);
            if state.set_authorization(&key, authorization)? {
                storage::save(&state_file, &state)?;
            }
            println!("{key}: {authorization}.");
        }
        Command::Revoke { repository, pr } => {
            let mut state = storage::load(&state_file)?;
            let key = PrKey::new(repository, pr)?;
            if state.set_authorization(&key, Authorization::AwaitingLead)? {
                storage::save(&state_file, &state)?;
            }
            println!("{key}: {}.", Authorization::AwaitingLead);
        }
        Command::Status { repository, pr } => {
            let state = storage::load(&state_file)?;
            let key = PrKey::new(repository, pr)?;
            let request = state
                .get(&key)
                .ok_or_else(|| StateError::RequestNotFound(key.clone()))?;
            println!("{}", request.key());
            println!("  head: {}", request.head_sha());
            println!("  state: {}", request.authorization());
        }
        Command::List => {
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
        Command::StatePath => println!("{}", state_file.display()),
    }
    Ok(())
}
