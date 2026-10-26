//! Exact-head GitHub check runs and commit statuses for operator display.

use super::{GithubClient, PullRequest};
use anyhow::{Context, Result, bail, ensure};
use review_platform::{HeadSha, PrKey};
use serde::Deserialize;
use std::fmt;

const PAGE_SIZE: usize = 100;
const MAX_RESULTS: usize = 1000;
const MAX_PAGES: usize = MAX_RESULTS / PAGE_SIZE;

pub struct CheckReport {
    check_runs: Vec<CheckRun>,
    commit_statuses: Vec<CommitStatus>,
    combined_state: Option<CommitState>,
}

impl CheckReport {
    pub fn check_runs(&self) -> &[CheckRun] {
        &self.check_runs
    }

    pub fn commit_statuses(&self) -> &[CommitStatus] {
        &self.commit_statuses
    }

    pub fn summary(&self) -> CheckSummary {
        let mut any = false;
        let mut pending = false;
        let mut unknown = false;
        for signal in self
            .check_runs
            .iter()
            .map(CheckRun::signal)
            .chain(self.commit_statuses.iter().map(CommitStatus::signal))
            .chain(self.combined_state.map(CommitState::signal))
        {
            any = true;
            match signal {
                Signal::Failing => return CheckSummary::Failing,
                Signal::Unknown => unknown = true,
                Signal::Pending => pending = true,
                Signal::Passing => {}
            }
        }
        if !any {
            CheckSummary::NoResults
        } else if unknown {
            CheckSummary::Unknown
        } else if pending {
            CheckSummary::Pending
        } else {
            CheckSummary::Green
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckSummary {
    Green,
    Pending,
    Failing,
    Unknown,
    NoResults,
}

impl fmt::Display for CheckSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Green => "green",
            Self::Pending => "pending",
            Self::Failing => "failing",
            Self::Unknown => "unknown",
            Self::NoResults => "no checks reported",
        };
        f.write_str(label)
    }
}

#[derive(Deserialize)]
struct CheckRunsPage {
    total_count: u64,
    check_runs: Vec<CheckRun>,
}

#[derive(Deserialize)]
pub struct CheckRun {
    pub name: String,
    head_sha: HeadSha,
    pub status: CheckRunStatus,
    pub conclusion: Option<CheckConclusion>,
}

impl CheckRun {
    fn signal(&self) -> Signal {
        match self.status {
            CheckRunStatus::Completed => match self.conclusion {
                Some(
                    CheckConclusion::Success | CheckConclusion::Neutral | CheckConclusion::Skipped,
                ) => Signal::Passing,
                Some(
                    CheckConclusion::Failure
                    | CheckConclusion::Cancelled
                    | CheckConclusion::TimedOut
                    | CheckConclusion::ActionRequired
                    | CheckConclusion::StartupFailure
                    | CheckConclusion::Stale,
                ) => Signal::Failing,
                Some(CheckConclusion::Unknown) | None => Signal::Unknown,
            },
            CheckRunStatus::Queued
            | CheckRunStatus::InProgress
            | CheckRunStatus::Waiting
            | CheckRunStatus::Requested
            | CheckRunStatus::Pending => Signal::Pending,
            CheckRunStatus::Unknown => Signal::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckRunStatus {
    Queued,
    InProgress,
    Completed,
    Waiting,
    Requested,
    Pending,
    #[serde(other)]
    Unknown,
}

impl CheckRunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::InProgress => "in progress",
            Self::Completed => "completed",
            Self::Waiting => "waiting",
            Self::Requested => "requested",
            Self::Pending => "pending",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckConclusion {
    Success,
    Failure,
    Neutral,
    Cancelled,
    Skipped,
    TimedOut,
    ActionRequired,
    StartupFailure,
    Stale,
    #[serde(other)]
    Unknown,
}

impl CheckConclusion {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Neutral => "neutral",
            Self::Cancelled => "cancelled",
            Self::Skipped => "skipped",
            Self::TimedOut => "timed out",
            Self::ActionRequired => "action required",
            Self::StartupFailure => "startup failure",
            Self::Stale => "stale",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Deserialize)]
struct CombinedStatusPage {
    sha: HeadSha,
    state: CommitState,
    total_count: u64,
    statuses: Vec<CommitStatus>,
}

#[derive(Deserialize)]
pub struct CommitStatus {
    pub context: String,
    pub state: CommitState,
}

impl CommitStatus {
    fn signal(&self) -> Signal {
        self.state.signal()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitState {
    Success,
    Failure,
    Pending,
    Error,
    #[serde(other)]
    Unknown,
}

impl CommitState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Pending => "pending",
            Self::Error => "error",
            Self::Unknown => "unknown",
        }
    }

    fn signal(self) -> Signal {
        match self {
            Self::Success => Signal::Passing,
            Self::Failure | Self::Error => Signal::Failing,
            Self::Pending => Signal::Pending,
            Self::Unknown => Signal::Unknown,
        }
    }
}

enum Signal {
    Passing,
    Pending,
    Failing,
    Unknown,
}

impl GithubClient {
    pub fn checks_for_pull(&self, key: &PrKey, pull: &PullRequest) -> Result<CheckReport> {
        let check_runs = self.check_runs_for_head(key, &pull.head_sha)?;
        let (commit_statuses, combined_state) =
            self.commit_statuses_for_head(key, &pull.head_sha)?;
        self.ensure_unchanged(key, pull, false)?;
        Ok(CheckReport {
            check_runs,
            commit_statuses,
            combined_state,
        })
    }

    fn check_runs_for_head(&self, key: &PrKey, head: &HeadSha) -> Result<Vec<CheckRun>> {
        let mut runs = Vec::new();
        let mut expected_total = None;
        for page in 1..=MAX_PAGES {
            let route = format!(
                "/repos/{}/commits/{head}/check-runs?per_page={PAGE_SIZE}&page={page}&filter=latest",
                key.repository()
            );
            let response = self.get(
                &route,
                "application/vnd.github+json",
                &format!("check runs for {key} at {head}"),
            )?;
            let data: CheckRunsPage = response
                .into_json()
                .with_context(|| format!("reading check runs for {key} at {head}"))?;
            check_total(&mut expected_total, data.total_count, "check runs")?;
            for run in data.check_runs {
                ensure!(
                    &run.head_sha == head,
                    "GitHub returned a check run for {} while inspecting {head}",
                    run.head_sha
                );
                runs.push(run);
            }
            if runs.len() as u64 == data.total_count {
                return Ok(runs);
            }
            ensure!(
                (runs.len() as u64) < data.total_count,
                "GitHub returned more check runs than reported for {head}"
            );
        }
        bail!("GitHub did not return every check run for {head}; retry later")
    }

    fn commit_statuses_for_head(
        &self,
        key: &PrKey,
        head: &HeadSha,
    ) -> Result<(Vec<CommitStatus>, Option<CommitState>)> {
        let mut statuses = Vec::new();
        let mut expected_total = None;
        let mut combined_state = None;
        for page in 1..=MAX_PAGES {
            let route = format!(
                "/repos/{}/commits/{head}/status?per_page={PAGE_SIZE}&page={page}",
                key.repository()
            );
            let response = self.get(
                &route,
                "application/vnd.github+json",
                &format!("commit statuses for {key} at {head}"),
            )?;
            let data: CombinedStatusPage = response
                .into_json()
                .with_context(|| format!("reading commit statuses for {key} at {head}"))?;
            ensure!(
                &data.sha == head,
                "GitHub returned commit statuses for {} while inspecting {head}",
                data.sha
            );
            check_total(&mut expected_total, data.total_count, "commit statuses")?;
            if let Some(previous) = combined_state {
                ensure!(
                    previous == data.state,
                    "GitHub commit statuses changed during pagination; retry later"
                );
            }
            combined_state = Some(data.state);
            statuses.extend(data.statuses);
            if statuses.len() as u64 == data.total_count {
                return Ok((
                    statuses,
                    if data.total_count == 0 {
                        None
                    } else {
                        combined_state
                    },
                ));
            }
            ensure!(
                (statuses.len() as u64) < data.total_count,
                "GitHub returned more commit statuses than reported for {head}"
            );
        }
        bail!("GitHub did not return every commit status for {head}; retry later")
    }
}

fn check_total(expected: &mut Option<u64>, actual: u64, what: &str) -> Result<()> {
    ensure!(
        actual <= MAX_RESULTS as u64,
        "GitHub reports more than {MAX_RESULTS} {what}; cannot summarize them safely"
    );
    if let Some(previous) = expected {
        ensure!(
            *previous == actual,
            "GitHub {what} changed during pagination; retry later"
        );
    } else {
        *expected = Some(actual);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(status: &str, conclusion: Option<&str>) -> CheckRun {
        let value = serde_json::json!({
            "name": "job",
            "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "status": status,
            "conclusion": conclusion
        });
        serde_json::from_value(value).unwrap()
    }

    fn report(runs: Vec<CheckRun>, statuses: &[&str]) -> CheckReport {
        let commit_statuses = statuses
            .iter()
            .map(|state| {
                serde_json::from_value(serde_json::json!({"context":"ci", "state":state})).unwrap()
            })
            .collect();
        CheckReport {
            check_runs: runs,
            commit_statuses,
            combined_state: None,
        }
    }

    #[test]
    fn summary_distinguishes_green_pending_failing_unknown_and_no_results() {
        assert_eq!(report(vec![], &[]).summary(), CheckSummary::NoResults);
        assert_eq!(
            report(vec![run("completed", Some("success"))], &[]).summary(),
            CheckSummary::Green
        );
        assert_eq!(
            report(vec![run("completed", Some("neutral"))], &[]).summary(),
            CheckSummary::Green
        );
        assert_eq!(
            report(vec![run("completed", Some("skipped"))], &[]).summary(),
            CheckSummary::Green
        );
        assert_eq!(
            report(vec![run("in_progress", None)], &[]).summary(),
            CheckSummary::Pending
        );
        assert_eq!(
            report(vec![run("completed", Some("failure"))], &[]).summary(),
            CheckSummary::Failing
        );
        assert_eq!(report(vec![], &["error"]).summary(), CheckSummary::Failing);
        assert_eq!(
            report(vec![run("completed", None)], &[]).summary(),
            CheckSummary::Unknown
        );
    }

    #[test]
    fn empty_commit_statuses_do_not_make_successful_check_runs_pending() {
        let report = CheckReport {
            check_runs: vec![run("completed", Some("success"))],
            commit_statuses: vec![],
            combined_state: None,
        };
        assert_eq!(report.summary(), CheckSummary::Green);
    }
}
