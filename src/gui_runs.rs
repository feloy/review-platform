//! Server-owned review jobs; browser requests observe but never own their lifetime.

use crate::{
    gui_diagnostics,
    review_commands::{InspectedRun, RunError},
};
use luthiai::{PrKey, review::RoundId};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use thiserror::Error;

pub type Run = fn(
    &Path,
    &Path,
    &PrKey,
    &InspectedRun,
    Arc<AtomicBool>,
    &mut dyn FnMut(&RoundId),
) -> Result<(), RunError>;

#[derive(Clone, Serialize)]
pub struct RunSummary {
    status: RunStatus,
    round_id: Option<RoundId>,
    failure: Option<RunFailure>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum RunStatus {
    Starting,
    Running,
    Finished,
    Failed,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum RunFailure {
    StalePreparation,
    Execution,
}

#[derive(Debug, Error)]
pub enum StartError {
    #[error("a GUI review job is already active for this PR")]
    Active,
    #[error("the GUI server is shutting down")]
    ShuttingDown,
    #[error("could not read GUI review jobs")]
    Unavailable,
}

/// One latest job per PR, including failures before a persistent round exists.
pub struct Runs {
    jobs: Mutex<HashMap<PrKey, RunSummary>>,
    cancelled: Arc<AtomicBool>,
    run: Run,
}

impl Runs {
    pub fn new(cancelled: Arc<AtomicBool>, run: Run) -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
            cancelled,
            run,
        }
    }

    pub fn is_shutting_down(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub fn summary(&self, key: &PrKey) -> Result<Option<RunSummary>, StartError> {
        Ok(self
            .jobs
            .lock()
            .map_err(|_| StartError::Unavailable)?
            .get(key)
            .cloned())
    }

    pub fn is_active(&self, key: &PrKey) -> Result<bool, StartError> {
        Ok(self
            .summary(key)?
            .is_some_and(|job| matches!(job.status, RunStatus::Starting | RunStatus::Running)))
    }

    pub fn forget(&self, key: &PrKey) -> Result<(), StartError> {
        let mut jobs = self.jobs.lock().map_err(|_| StartError::Unavailable)?;
        if jobs
            .get(key)
            .is_some_and(|job| matches!(job.status, RunStatus::Starting | RunStatus::Running))
        {
            return Err(StartError::Active);
        }
        jobs.remove(key);
        Ok(())
    }

    pub fn start(
        self: &Arc<Self>,
        state_file: PathBuf,
        config_file: PathBuf,
        key: PrKey,
        inspected: InspectedRun,
    ) -> Result<(), StartError> {
        {
            let mut jobs = self.jobs.lock().map_err(|_| StartError::Unavailable)?;
            if self.is_shutting_down() {
                return Err(StartError::ShuttingDown);
            }
            if jobs
                .get(&key)
                .is_some_and(|job| matches!(job.status, RunStatus::Starting | RunStatus::Running))
            {
                return Err(StartError::Active);
            }
            jobs.insert(
                key.clone(),
                RunSummary {
                    status: RunStatus::Starting,
                    round_id: None,
                    failure: None,
                },
            );
        }
        let worker = self.clone();
        let worker_key = key.clone();
        let task = tokio::task::spawn_blocking(move || {
            (worker.run)(
                &state_file,
                &config_file,
                &worker_key,
                &inspected,
                worker.cancelled.clone(),
                &mut |id| {
                    if let Ok(mut jobs) = worker.jobs.lock() {
                        if let Some(job) = jobs.get_mut(&worker_key) {
                            job.status = RunStatus::Running;
                            job.round_id = Some(id.clone());
                        }
                    }
                },
            )
        });
        // This supervisor is detached from the HTTP response and SSE connections.
        let owner = self.clone();
        tokio::spawn(async move {
            let result = task.await;
            let round_id = owner
                .summary(&key)
                .ok()
                .flatten()
                .and_then(|job| job.round_id);
            let failure = match result {
                Ok(Ok(())) => None,
                Ok(Err(error)) => {
                    gui_diagnostics::report("review", &key, round_id.as_ref(), &error);
                    Some(match error {
                        RunError::Inspection(_) => RunFailure::StalePreparation,
                        RunError::Execution(_) => RunFailure::Execution,
                    })
                }
                Err(error) => {
                    gui_diagnostics::report("review", &key, round_id.as_ref(), &error);
                    Some(RunFailure::Execution)
                }
            };
            if let Ok(mut jobs) = owner.jobs.lock() {
                if let Some(job) = jobs.get_mut(&key) {
                    job.status = if failure.is_some() {
                        RunStatus::Failed
                    } else {
                        RunStatus::Finished
                    };
                    job.failure = failure;
                }
            }
        });
        Ok(())
    }
}
