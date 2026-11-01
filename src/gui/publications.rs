//! Server-owned publication jobs and safe progress summaries for the operator GUI.

use crate::{
    gui_diagnostics,
    publication_commands::{self, PublicationReport, PublishError},
};
use review_platform::{
    PrKey,
    publication::{PublicationHash, PublicationStateError},
    review::{Freshness, RoundId, RoundStateError},
};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use thiserror::Error;

pub(super) type Publish =
    fn(&Path, &Path, &PrKey, &RoundId, &PublicationHash) -> Result<PublicationReport, PublishError>;

#[derive(Clone, Serialize)]
pub(super) struct PublicationSummary {
    round_id: RoundId,
    status: PublicationStatus,
    error: Option<&'static str>,
    freshness: Option<Freshness>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum PublicationStatus {
    Active,
    Published,
    Failed,
    Unknown,
}

#[derive(Debug, Error)]
pub(super) enum StartError {
    #[error("a GUI publication is already active for this PR")]
    Active,
    #[error("could not read GUI publication jobs")]
    Unavailable,
}

pub(super) struct Publications {
    jobs: Mutex<HashMap<PrKey, PublicationSummary>>,
    publish: Publish,
}

impl Publications {
    pub(super) fn new(publish: Publish) -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
            publish,
        }
    }

    pub(super) fn summary(&self, key: &PrKey) -> Result<Option<PublicationSummary>, StartError> {
        Ok(self
            .jobs
            .lock()
            .map_err(|_| StartError::Unavailable)?
            .get(key)
            .cloned())
    }

    pub(super) fn is_active(&self, key: &PrKey) -> Result<bool, StartError> {
        Ok(self
            .summary(key)?
            .is_some_and(|job| matches!(job.status, PublicationStatus::Active)))
    }

    pub(super) fn start(
        self: &Arc<Self>,
        state_file: PathBuf,
        config_file: PathBuf,
        key: PrKey,
        id: RoundId,
        hash: PublicationHash,
    ) -> Result<(), StartError> {
        {
            let mut jobs = self.jobs.lock().map_err(|_| StartError::Unavailable)?;
            if jobs
                .get(&key)
                .is_some_and(|job| matches!(job.status, PublicationStatus::Active))
            {
                return Err(StartError::Active);
            }
            jobs.insert(
                key.clone(),
                PublicationSummary {
                    round_id: id.clone(),
                    status: PublicationStatus::Active,
                    error: None,
                    freshness: None,
                },
            );
        }
        let publish = self.publish;
        let worker_key = key.clone();
        let worker_id = id.clone();
        let task = tokio::task::spawn_blocking(move || {
            publish(&state_file, &config_file, &worker_key, &worker_id, &hash)
        });
        let owner = self.clone();
        // The supervisor outlives the HTTP response and every browser connection.
        tokio::spawn(async move {
            let (status, error, freshness) = match task.await {
                Ok(Ok(report)) => (PublicationStatus::Published, None, report.freshness()),
                Ok(Err(error)) => {
                    gui_diagnostics::report("publication", &key, Some(&id), &error);
                    (
                        if matches!(error, PublishError::OutcomeUnknown(_)) {
                            PublicationStatus::Unknown
                        } else {
                            PublicationStatus::Failed
                        },
                        Some(public_message(&error)),
                        None,
                    )
                }
                Err(error) => {
                    gui_diagnostics::report("publication", &key, Some(&id), &error);
                    (
                        PublicationStatus::Unknown,
                        Some(
                            "Publication stopped unexpectedly. Inspect the recorded round and recover any pending publication before deciding again.",
                        ),
                        None,
                    )
                }
            };
            if let Ok(mut jobs) = owner.jobs.lock() {
                if let Some(job) = jobs.get_mut(&key) {
                    job.status = status;
                    job.error = error;
                    job.freshness = freshness;
                }
            }
        });
        Ok(())
    }
}

pub(super) fn public_message(error: &PublishError) -> &'static str {
    match error {
        PublishError::Draft(_) => {
            "Could not verify the private draft against its original output and publication seal. Check the saved round files."
        }
        PublishError::Round(RoundStateError::CleanupPending(_)) => {
            "Sandbox cleanup must finish before publishing this draft."
        }
        PublishError::Round(RoundStateError::State(_)) | PublishError::Stale => {
            "PR inputs changed or the PR closed. Reopen the draft and inspect current state before publishing."
        }
        PublishError::Round(_) => {
            "This round cannot be published in its current state. Reopen it and recover any pending publication before deciding again."
        }
        PublishError::Body(PublicationStateError::BodyTooLong { .. }) => {
            "This draft exceeds GitHub's 65,536-character review limit. Reject it and run another round."
        }
        PublishError::Body(_) => {
            "The publication body does not match the inspected hash. Reopen the draft before deciding."
        }
        PublishError::Config(_) | PublishError::ReadConfig(_) => {
            "Could not load publication configuration. Check the GUI server's configuration file."
        }
        PublishError::Publisher(_) => {
            "Could not verify the publisher or GitHub rejected publication. Check publisher_login and the server's GitHub write credentials and access."
        }
        PublishError::Github(_) => {
            "Could not verify current PR inputs on GitHub. Check read access and reopen the draft before publishing."
        }
        PublishError::Local(_) => {
            "Could not update local publication state. Inspect the recorded round before deciding again."
        }
        PublishError::OutcomeUnknown(_) => {
            "Publication outcome is unknown. Check GitHub and recover this publication through the CLI before deciding again."
        }
    }
}

impl Default for Publications {
    fn default() -> Self {
        Self::new(publication_commands::publish)
    }
}
