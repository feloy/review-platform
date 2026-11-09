//! Agent adapters and shared process supervision for constrained review rounds.

mod claude;
mod codex;

use crate::archive::StagedArchive;
use crate::config::{
    AgentKind, AgentRunnerConfig, ExecutionConfig, PromptHash, valid_vm_absolute_path,
};
use crate::openshell::OpenShell;
use crate::review::{OUTPUT_SCHEMA, REVIEW_RULES, RoundRecord};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use thiserror::Error;

pub use claude::ClaudeAdapter;
pub use codex::CodexAdapter;

/// Agent-specific behavior; workflow gates and sandbox lifecycle remain shared.
pub trait AgentAdapter: Sync {
    fn prepare(
        &self,
        image: &StagedArchive,
        runner: &AgentRunnerConfig,
    ) -> Result<PreparedAgent, AgentPreparationError>;
    fn tool_guidance(&self) -> &'static str;
    fn command(&self, invocation: &AgentInvocation<'_>) -> Vec<OsString>;
    fn capture(&self, reader: &mut dyn Read, limit: u64, max_turns: u32) -> AgentRun;
}

pub fn adapter(kind: AgentKind) -> &'static dyn AgentAdapter {
    match kind {
        AgentKind::Claude => &ClaudeAdapter,
        AgentKind::Codex => &CodexAdapter,
    }
}

#[derive(Debug, Error)]
pub enum AgentPreparationError {
    #[error("VM image configuration lacks an absolute {variable} path")]
    MissingExecutable { variable: &'static str },
    #[error(
        "Codex requires an absolute HTTP(S) OPENAI_BASE_URL without credentials, query, or fragment in image metadata or runner.environment"
    )]
    InvalidCodexEndpoint,
}

/// Executable and mandatory environment derived from a verified image and adapter.
pub struct PreparedAgent {
    executable: String,
    environment: BTreeMap<String, String>,
    arguments: Vec<OsString>,
}

impl PreparedAgent {
    fn from_image(
        image: &StagedArchive,
        variable: &'static str,
        environment: &[(&str, &str)],
    ) -> Result<Self, AgentPreparationError> {
        let executable = image
            .environment()
            .get(variable)
            .filter(|value| valid_vm_absolute_path(value))
            .ok_or(AgentPreparationError::MissingExecutable { variable })?;
        Ok(Self {
            executable: executable.clone(),
            arguments: Vec::new(),
            environment: environment
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
        })
    }

    pub fn executable(&self) -> &str {
        &self.executable
    }
    pub fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }
}

pub(crate) fn reserved_environment_key(key: &str) -> bool {
    matches!(
        key,
        "CLAUDE_CODE_EXECUTABLE"
            | "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY"
            | "CODEX_PATH"
            | "CODEX_HOME"
    )
}

pub fn review_rules(kind: AgentKind) -> String {
    format!("{REVIEW_RULES}\n{}", adapter(kind).tool_guidance())
}

pub fn rules_hash(kind: AgentKind) -> PromptHash {
    PromptHash::of(&format!("{}\n{OUTPUT_SCHEMA}", review_rules(kind)))
}

pub fn compose_system_prompt(kind: AgentKind, configured: &str) -> String {
    format!("{configured}\n\n{}", review_rules(kind))
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum AgentRunError {
    #[error("starting the review agent failed")]
    Launch,
    #[error("the review agent exited unsuccessfully")]
    Unsuccessful,
    #[error("the review timed out")]
    Timeout,
    #[error("the review was interrupted")]
    Interrupted,
    #[error("the review log exceeded its configured limit")]
    LogTooLarge,
    #[error("the review log was not valid UTF-8")]
    InvalidUtf8,
    #[error("the agent exceeded its configured turn limit")]
    TooManyTurns,
    #[error("the agent output stream was invalid or incomplete")]
    InvalidEvent,
    #[error("reading the review log failed")]
    ReadLog,
}

pub struct AgentRun {
    pub log: String,
    pub result: Result<(), AgentRunError>,
    pub final_response: Option<String>,
}

/// Trusted command inputs for one selected review round.
pub struct AgentInvocation<'a> {
    prepared: &'a PreparedAgent,
    round: &'a RoundRecord,
    prompt_path: &'a str,
    base_sha: &'a str,
}

impl<'a> AgentInvocation<'a> {
    pub fn new(
        prepared: &'a PreparedAgent,
        round: &'a RoundRecord,
        prompt_path: &'a str,
        base_sha: &'a str,
    ) -> Self {
        Self {
            prepared,
            round,
            prompt_path,
            base_sha,
        }
    }
}

pub fn run(
    shell: &OpenShell<'_>,
    config: &ExecutionConfig,
    runner: &AgentRunnerConfig,
    invocation: &AgentInvocation<'_>,
    cancelled: Arc<AtomicBool>,
) -> AgentRun {
    let adapter = adapter(runner.kind());
    let command = adapter.command(invocation);
    let mut process = shell.exec_review_agent(invocation.round, &command);
    process
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(_) => {
            return AgentRun {
                log: String::new(),
                result: Err(AgentRunError::Launch),
                final_response: None,
            };
        }
    };
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return AgentRun {
            log: String::new(),
            result: Err(AgentRunError::Launch),
            final_response: None,
        };
    };
    let Some(mut stderr) = child.stderr.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return AgentRun {
            log: String::new(),
            result: Err(AgentRunError::Launch),
            final_response: None,
        };
    };
    let (sender, receiver) = mpsc::channel();
    let limit = config.log_limit_bytes();
    let turns = runner.max_turns();
    let reader = std::thread::spawn(move || {
        let capture = adapter.capture(&mut stdout, limit, turns);
        let _ = sender.send(capture);
    });
    let stderr_reader = std::thread::spawn(move || read_limited(&mut stderr, limit));
    let deadline = Instant::now() + Duration::from_secs(config.timeout_seconds());
    let (capture, process_result) = loop {
        if cancelled.load(Ordering::Relaxed) {
            let _ = child.kill();
            break (receiver.recv().ok(), Err(AgentRunError::Interrupted));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            break (receiver.recv().ok(), Err(AgentRunError::Timeout));
        }
        match receiver.try_recv() {
            Ok(capture) if capture.result.is_err() => {
                let _ = child.kill();
                break (Some(capture), Ok(()));
            }
            Ok(capture) => {
                let status = child.wait();
                break (
                    Some(capture),
                    if status.is_ok_and(|status| status.success()) {
                        Ok(())
                    } else {
                        Err(AgentRunError::Unsuccessful)
                    },
                );
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                let _ = child.kill();
                break (None, Err(AgentRunError::ReadLog));
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = child.wait();
    let _ = reader.join();
    let stderr = stderr_reader.join().unwrap_or(Err(AgentRunError::ReadLog));
    let capture = capture.unwrap_or_else(|| AgentRun {
        log: String::new(),
        result: Err(AgentRunError::ReadLog),
        final_response: None,
    });
    with_stderr(
        AgentRun {
            log: capture.log,
            final_response: capture.final_response,
            result: process_result.and(capture.result),
        },
        stderr,
        limit,
    )
}

fn with_stderr(mut run: AgentRun, stderr: Result<String, AgentRunError>, limit: u64) -> AgentRun {
    let result = match stderr {
        Ok(_) => run.result,
        Err(error) => Err(error),
    };
    if let Ok(stderr) = stderr {
        if !stderr.is_empty() {
            let mut log = Log {
                text: run.log,
                limit,
            };
            if let Err(error) = log.append(&format!("\nAgent stderr:\n{stderr}\n")) {
                run.log = log.text;
                run.result = Err(error);
                run.final_response = None;
                return run;
            }
            run.log = log.text;
        }
    }
    run.result = result;
    run.final_response = if result.is_ok() {
        run.final_response
    } else {
        None
    };
    run
}

fn read_limited(mut reader: impl Read, limit: u64) -> Result<String, AgentRunError> {
    let mut bytes = Vec::new();
    let mut exceeded_limit = false;
    let mut buffer = [0_u8; 8192];
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(count) => count,
            Err(_) => return Err(AgentRunError::ReadLog),
        };
        if count == 0 {
            break;
        }
        if bytes.len() as u64 + count as u64 <= limit {
            bytes.extend_from_slice(&buffer[..count]);
        } else {
            exceeded_limit = true;
        }
    }
    if exceeded_limit {
        return Err(AgentRunError::LogTooLarge);
    }
    String::from_utf8(bytes).map_err(|_| AgentRunError::InvalidUtf8)
}

struct Log {
    text: String,
    limit: u64,
}

impl Log {
    fn append(&mut self, value: &str) -> Result<(), AgentRunError> {
        if self.text.len() as u64 + value.len() as u64 > self.limit {
            return Err(AgentRunError::LogTooLarge);
        }
        self.text.push_str(value);
        Ok(())
    }
}

fn capture_lines(
    mut reader: impl Read,
    limit: u64,
    mut render: impl FnMut(&str, &mut Log) -> Result<(), AgentRunError>,
) -> AgentRun {
    let mut log = Log {
        text: String::new(),
        limit,
    };
    let mut pending = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(count) => count,
            Err(_) => {
                return AgentRun {
                    log: log.text,
                    result: Err(AgentRunError::ReadLog),
                    final_response: None,
                };
            }
        };
        if count == 0 {
            break;
        }
        pending.extend_from_slice(&buffer[..count]);
        while let Some(index) = pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = pending.drain(..=index).collect();
            let line = match String::from_utf8(line) {
                Ok(line) => line,
                Err(_) => {
                    return AgentRun {
                        log: log.text,
                        result: Err(AgentRunError::InvalidUtf8),
                        final_response: None,
                    };
                }
            };
            if let Err(error) = render(&line, &mut log) {
                return AgentRun {
                    log: log.text,
                    result: Err(error),
                    final_response: None,
                };
            }
        }
        if pending.len() as u64 > limit.saturating_mul(16).max(1024 * 1024) {
            return AgentRun {
                log: log.text,
                result: Err(AgentRunError::LogTooLarge),
                final_response: None,
            };
        }
    }
    if !pending.is_empty() {
        let line = match String::from_utf8(pending) {
            Ok(line) => line,
            Err(_) => {
                return AgentRun {
                    log: log.text,
                    result: Err(AgentRunError::InvalidUtf8),
                    final_response: None,
                };
            }
        };
        if let Err(error) = render(&line, &mut log) {
            return AgentRun {
                log: log.text,
                result: Err(error),
                final_response: None,
            };
        }
    }
    AgentRun {
        log: log.text,
        result: Ok(()),
        final_response: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ReviewConfig;
    use crate::review::RoundId;
    use crate::{HeadSha, InputRevision, ObservedDiscussion, PrKey};
    use std::num::NonZeroU64;
    use std::path::{Path, PathBuf};

    pub(super) fn test_round(kind: AgentKind) -> (ReviewConfig, RoundRecord) {
        let mut runner = crate::config::test_runner_json();
        runner["kind"] = kind.as_str().into();
        let raw = serde_json::json!({
            "schema_version": 6,
            "execution": crate::config::test_execution_json(),
            "models": [{"id": "model-alpha", "coauthors": []}],
            "agents": [{"id": "reviewer", "models": ["model-alpha"],
                "prompt_file": crate::config::test_prompt_file("review.md"), "runner": runner}],
            "repositories": [{"name": "owner/repo", "review_models": ["model-alpha"],
                "runners": [{"agent": "reviewer", "vm_archive": {
                    "path": std::env::temp_dir().join("reviewer.tar"),
                    "sha256": "a".repeat(64), "config_sha256": "b".repeat(64)
                }}]}]
        });
        let config =
            ReviewConfig::from_json(&serde_json::to_vec(&raw).unwrap(), Path::new("config.json"))
                .unwrap();
        let key = PrKey::new("owner/repo", 1).unwrap();
        let choices = config.review_models_for(&key).unwrap();
        let round = RoundRecord::new(
            RoundId::new("r-test").unwrap(),
            key,
            HeadSha::new("a".repeat(40)).unwrap(),
            InputRevision::new(1).unwrap(),
            ObservedDiscussion::new(NonZeroU64::new(1).unwrap(), vec![]).unwrap(),
            &choices[0],
            1,
        );
        (config, round)
    }

    #[test]
    fn adapters_require_their_own_verified_image_executable() {
        for (kind, variable, executable) in [
            (
                AgentKind::Claude,
                "CLAUDE_CODE_EXECUTABLE",
                "/usr/bin/claude",
            ),
            (AgentKind::Codex, "CODEX_PATH", "/usr/bin/codex"),
        ] {
            let (config, round) = test_round(kind);
            let choices = config.review_models_for(round.key()).unwrap();
            let runner = choices[0].agent().runner();
            for value in [None, Some("bin/relative"), Some("/usr/../bin/agent")] {
                let image = StagedArchive::for_test(
                    PathBuf::from("/tmp/image.tar"),
                    value
                        .map(|value| BTreeMap::from([(variable.into(), value.into())]))
                        .unwrap_or_default(),
                );
                assert!(matches!(adapter(kind).prepare(&image, runner),
                    Err(AgentPreparationError::MissingExecutable { variable: actual }) if actual == variable));
            }
            let image = StagedArchive::for_test(
                PathBuf::from("/tmp/image.tar"),
                BTreeMap::from([
                    (variable.into(), executable.into()),
                    (
                        "OPENAI_BASE_URL".into(),
                        "https://gateway.example/v1".into(),
                    ),
                ]),
            );
            assert_eq!(
                adapter(kind).prepare(&image, runner).unwrap().executable(),
                executable
            );
        }
    }

    #[test]
    fn trusted_rules_and_round_fingerprints_use_the_selected_adapter() {
        assert_ne!(rules_hash(AgentKind::Claude), rules_hash(AgentKind::Codex));
        for kind in [AgentKind::Claude, AgentKind::Codex] {
            let (_, round) = test_round(kind);
            let prompt = compose_system_prompt(kind, "Configured review instructions.");
            assert!(prompt.starts_with("Configured review instructions.\n\n"));
            assert!(prompt.contains(REVIEW_RULES));
            assert!(prompt.ends_with(adapter(kind).tool_guidance()));
            assert_eq!(round.format_hash(), &rules_hash(kind));
        }
        let codex = compose_system_prompt(AgentKind::Codex, "Review the PR.");
        assert!(!codex.contains("dontAsk"));
        assert!(!codex.contains("Use Edit"));
    }

    #[test]
    fn stderr_is_preserved_when_the_event_stream_is_invalid() {
        let run = AgentRun {
            log: String::from("stdout log\n"),
            result: Err(AgentRunError::InvalidEvent),
            final_response: None,
        };
        let run = with_stderr(run, Ok(String::from("unexpected warning")), 1024);

        assert_eq!(run.result, Err(AgentRunError::InvalidEvent));
        assert!(run.log.contains("stdout log\n"));
        assert!(run.log.contains("\nAgent stderr:\nunexpected warning\n"));
        assert_eq!(run.final_response, None);
    }

    #[test]
    fn stderr_obeys_the_combined_log_limit() {
        let run = AgentRun {
            log: String::from("stdout log\n"),
            result: Err(AgentRunError::InvalidEvent),
            final_response: None,
        };
        let run = with_stderr(run, Ok(String::from("stderr")), 17);

        assert_eq!(run.result, Err(AgentRunError::LogTooLarge));
        assert_eq!(run.final_response, None);
    }
}
