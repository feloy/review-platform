//! Local GUI job diagnostics, separate from browser-safe progress summaries.

use crate::escape_terminal_field;
use review_platform::{PrKey, review::RoundId};
use std::{error::Error, io::Write};

pub fn report(job: &str, key: &PrKey, id: Option<&RoundId>, error: &(dyn Error + 'static)) {
    // A closed diagnostic stream must not prevent the job's final state update.
    let _ = writeln!(
        std::io::stderr(),
        "{}",
        failure_message(job, key, id, error)
    );
}

fn failure_message(
    job: &str,
    key: &PrKey,
    id: Option<&RoundId>,
    error: &(dyn Error + 'static),
) -> String {
    let round = id.map_or_else(
        || "before round creation".into(),
        |id| format!("round {id}"),
    );
    let details = if let Some(error) = error.downcast_ref::<tokio::task::JoinError>() {
        // JoinError's Display includes arbitrary panic payloads.
        format!(
            "worker task {} {}",
            error.id(),
            if error.is_panic() {
                "panicked (payload omitted)"
            } else {
                "was cancelled"
            }
        )
    } else {
        let causes: Vec<_> =
            std::iter::successors(Some(error), |error| (*error).source()).collect();
        if let Some(json) = causes
            .iter()
            .find_map(|error| error.downcast_ref::<serde_json::Error>())
            .filter(|error| !error.is_io())
        {
            // Serde errors can quote draft text, prompts, or response values.
            // Ancestor Display implementations can embed that same text too.
            format!(
                "invalid JSON ({:?}) at line {}, column {} (input values omitted)",
                json.classify(),
                json.line(),
                json.column()
            )
        } else {
            causes
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(": ")
        }
    };
    escape_terminal_field(&format!("GUI {job} for {key} {round} failed: {details}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        publication_commands::PublishError,
        review_commands::{InspectionError, RunError},
    };
    use review_platform::{
        config::ConfigError, github::publisher::PublisherError, review::ArtifactError,
    };

    fn message(error: &(dyn Error + 'static)) -> String {
        failure_message(
            "publication",
            &PrKey::new("owner/repo", 15).unwrap(),
            Some(&RoundId::new("r-gui").unwrap()),
            error,
        )
    }

    #[test]
    fn review_failures_identify_preflight_and_keep_execution_causes() {
        let key = PrKey::new("owner/repo", 15).unwrap();
        let diagnostic = failure_message(
            "review",
            &key,
            None,
            &RunError::Inspection(InspectionError::RulesChanged),
        );
        assert!(diagnostic.contains("GUI review for owner/repo#15 before round creation failed:"));
        assert!(diagnostic.contains("authorization or configuration changed"));

        let cause = anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            .context("saving round state");
        let diagnostic = message(&RunError::Execution(cause.into()));
        assert!(diagnostic.contains("round r-gui"));
        assert!(diagnostic.contains("saving round state"));
        assert!(diagnostic.contains("permission denied"));
    }

    #[test]
    fn publication_failures_keep_actionable_causes_and_escape_terminal_controls() {
        for (error, expected) in [
            (
                PublisherError::MissingToken,
                "REVIEW_PLATFORM_GITHUB_TOKEN_RW",
            ),
            (
                PublisherError::WrongPublisher {
                    expected: "publisher".into(),
                    actual: "other-user".into(),
                },
                "write token belongs to other-user, expected publisher",
            ),
            (
                PublisherError::HttpStatus {
                    operation: "review publication",
                    status: 403,
                },
                "HTTP 403",
            ),
            (
                PublisherError::Transport {
                    operation: "review publication",
                    source: std::io::Error::from(std::io::ErrorKind::TimedOut).into(),
                },
                "timed out",
            ),
            (
                PublisherError::InvalidResponse {
                    operation: "review publication",
                    detail: "review identity, commit, state, or body differs from the request"
                        .into(),
                },
                "body differs from the request",
            ),
        ] {
            let diagnostic = message(&PublishError::Publisher(error));
            assert!(diagnostic.contains("GUI publication for owner/repo#15 round r-gui failed:"));
            assert!(diagnostic.contains(expected), "{diagnostic}");
        }
        let cause = anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            .context("saving publication receipt\n\u{1b}[31m");
        let diagnostic = message(&PublishError::OutcomeUnknown(cause.into()));
        assert!(diagnostic.contains("publication outcome unknown"));
        assert!(diagnostic.contains("saving publication receipt"));
        assert!(diagnostic.contains("permission denied"));
        assert!(!diagnostic.chars().any(char::is_control));
    }

    fn private_json_error() -> serde_json::Error {
        serde_json::from_str::<bool>(r#""PRIVATE_DRAFT_PROMPT_OR_TOKEN""#).unwrap_err()
    }

    #[test]
    fn json_write_errors_keep_the_storage_failure() {
        struct Unwritable;
        impl std::io::Write for Unwritable {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::PermissionDenied.into())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let error = serde_json::to_writer(Unwritable, &"PRIVATE_SAVED_STATE").unwrap_err();
        let error = anyhow::Error::new(error).context("serializing state");
        let diagnostic = message(&PublishError::OutcomeUnknown(error.into()));
        assert!(diagnostic.contains("serializing state"));
        assert!(diagnostic.contains("permission denied"));
        assert!(!diagnostic.contains("PRIVATE_SAVED_STATE"));
    }

    #[test]
    fn json_diagnostics_omit_private_values_even_when_embedded_by_parent_errors() {
        let errors: Vec<Box<dyn Error>> = vec![
            Box::new(RunError::Execution(
                anyhow::Error::new(ArtifactError::Json(private_json_error()))
                    .context("validating agent output")
                    .into(),
            )),
            Box::new(RunError::Inspection(InspectionError::Config(
                ConfigError::Json(private_json_error()),
            ))),
            Box::new(PublishError::Publisher(PublisherError::Json {
                operation: "review publication",
                source: private_json_error(),
            })),
            Box::new(PublishError::OutcomeUnknown(
                anyhow::Error::new(private_json_error())
                    .context("parsing state file")
                    .into(),
            )),
        ];
        for error in errors {
            let diagnostic = message(error.as_ref());
            assert!(diagnostic.contains("invalid JSON (Data) at line 1, column"));
            assert!(!diagnostic.contains("PRIVATE_DRAFT_PROMPT_OR_TOKEN"));
        }
    }

    #[test]
    fn join_failures_report_panic_or_cancellation_without_panic_payloads() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let error = tokio::task::spawn_blocking(|| panic!("PRIVATE_PANIC_PAYLOAD"))
                    .await
                    .unwrap_err();
                let diagnostic = message(&error);
                assert!(diagnostic.contains("worker task"));
                assert!(diagnostic.contains("panicked (payload omitted)"));
                assert!(!diagnostic.contains("PRIVATE_PANIC_PAYLOAD"));

                let task = tokio::spawn(std::future::pending::<()>());
                task.abort();
                let diagnostic = message(&task.await.unwrap_err());
                assert!(diagnostic.contains("was cancelled"));
            });
    }
}
