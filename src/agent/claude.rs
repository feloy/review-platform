//! Claude Code command permissions, trusted tool guidance, and stream interpretation.

use super::{
    AgentAdapter, AgentInvocation, AgentPreparationError, AgentRun, AgentRunError, Log,
    PreparedAgent, capture_lines,
};
use crate::archive::StagedArchive;
use crate::config::AgentRunnerConfig;
use crate::review::RoundRecord;
use crate::sandbox_layout;
use serde_json::Value;
use std::collections::HashSet;
use std::ffi::OsString;
use std::io::Read;

pub(super) const TOOL_GUIDANCE: &str = include_str!("../prompts/claude.md");

pub struct ClaudeAdapter;

impl AgentAdapter for ClaudeAdapter {
    fn prepare(
        &self,
        image: &StagedArchive,
        _runner: &AgentRunnerConfig,
    ) -> Result<PreparedAgent, AgentPreparationError> {
        PreparedAgent::from_image(
            image,
            "CLAUDE_CODE_EXECUTABLE",
            &[("CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY", "1")],
        )
    }

    fn tool_guidance(&self) -> &'static str {
        TOOL_GUIDANCE
    }

    fn command(&self, invocation: &AgentInvocation<'_>) -> Vec<OsString> {
        command_arguments(
            invocation.prepared.executable(),
            invocation.round,
            invocation.prompt_path,
            invocation.task_prompt,
        )
    }

    fn capture(&self, reader: &mut dyn Read, limit: u64, max_turns: u32) -> AgentRun {
        let mut renderer = Renderer::new(max_turns);
        let mut run = capture_lines(reader, limit, |line, log| renderer.render(line, log));
        if run.result.is_ok() && (!renderer.finished || renderer.final_response.is_none()) {
            run.result = Err(AgentRunError::InvalidEvent);
        }
        run.final_response = if run.result.is_ok() {
            renderer.final_response
        } else {
            None
        };
        run
    }
}

fn command_arguments(
    executable: &str,
    round: &RoundRecord,
    prompt_path: &str,
    task_prompt: &str,
) -> Vec<OsString> {
    let checkout = sandbox_layout::CHECKOUT_TARGET;
    let allowed = format!(
        "Read(./src/**),Read(./instructions/**),Bash(git -C {checkout} diff *),Bash(git -C {checkout} log *),Bash(git -C {checkout} show *),Bash(git -C {checkout} status *),Bash(gh api --method GET *)"
    );
    vec![
        executable.into(),
        "--bare".into(),
        "--safe-mode".into(),
        "--disable-slash-commands".into(),
        "--strict-mcp-config".into(),
        "--setting-sources".into(),
        "".into(),
        "--no-session-persistence".into(),
        "--permission-mode".into(),
        "dontAsk".into(),
        "--tools".into(),
        "Read,Bash".into(),
        "--allowedTools".into(),
        allowed.into(),
        "--model".into(),
        round.model().as_str().into(),
        "--append-system-prompt-file".into(),
        prompt_path.into(),
        "--output-format".into(),
        "stream-json".into(),
        "--json-schema".into(),
        crate::review::OUTPUT_SCHEMA.into(),
        "--verbose".into(),
        "--include-partial-messages".into(),
        "--print".into(),
        task_prompt.into(),
    ]
}

struct Renderer {
    max_turns: u32,
    turns: u32,
    counted_turn_ids: HashSet<String>,
    open_text: HashSet<String>,
    streamed_text: HashSet<String>,
    finished: bool,
    final_response: Option<String>,
}

impl Renderer {
    fn new(max_turns: u32) -> Self {
        Self {
            max_turns,
            turns: 0,
            counted_turn_ids: HashSet::new(),
            open_text: HashSet::new(),
            streamed_text: HashSet::new(),
            finished: false,
            final_response: None,
        }
    }

    fn render(&mut self, line: &str, log: &mut Log) -> Result<(), AgentRunError> {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            return log.append(line);
        };
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        let key = event
            .get("parent_tool_use_id")
            .and_then(Value::as_str)
            .unwrap_or("main")
            .to_owned();
        let who = if key == "main" { "Claude" } else { "Subagent" };
        match kind {
            "system" => {
                if event.get("subtype").and_then(Value::as_str) == Some("init") {
                    log.append("Claude session started\n")?;
                }
            }
            "stream_event" => {
                let stream = &event["event"];
                if stream["delta"]["type"] == "text_delta" {
                    if self.open_text.insert(key.clone()) {
                        log.append(&format!("\n{who}:\n"))?;
                    }
                    if let Some(text) = stream["delta"]["text"].as_str() {
                        log.append(text)?;
                    }
                    self.streamed_text.insert(key);
                } else if stream["type"] == "content_block_stop" && self.open_text.remove(&key) {
                    log.append("\n")?;
                }
            }
            "assistant" => {
                if self.open_text.remove(&key) {
                    log.append("\n")?;
                }
                if key == "main" {
                    let message_id = event["message"]["id"].as_str().filter(|id| !id.is_empty());
                    let new_turn = message_id
                        .map(|id| self.counted_turn_ids.insert(id.to_owned()))
                        .unwrap_or(true);
                    if new_turn {
                        self.turns += 1;
                        if self.turns > self.max_turns {
                            return Err(AgentRunError::TooManyTurns);
                        }
                    }
                }
                if let Some(blocks) = event["message"]["content"].as_array() {
                    for block in blocks {
                        match block["type"].as_str() {
                            Some("text") if !self.streamed_text.contains(&key) => {
                                if let Some(text) = block["text"].as_str() {
                                    log.append(&format!("\n{who}:\n{text}\n"))?;
                                }
                            }
                            Some("tool_use") => {
                                let name = block["name"].as_str().unwrap_or("tool");
                                log.append(&format!("\n{who} tool {name}: {}\n", block["input"]))?;
                            }
                            _ => {}
                        }
                    }
                }
                self.streamed_text.remove(&key);
            }
            "user" => {
                if let Some(blocks) = event["message"]["content"].as_array() {
                    for block in blocks {
                        match block["type"].as_str() {
                            Some("tool_result") => {
                                log.append(&format!("Tool result: {}\n", block["content"]))?
                            }
                            Some("text") => {
                                if let Some(text) = block["text"].as_str() {
                                    log.append(&format!("User: {text}\n"))?;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            "result" => {
                if event["is_error"] != false || event["subtype"] != "success" {
                    log.append("Claude failed\n")?;
                    return Err(AgentRunError::Unsuccessful);
                }
                // With --json-schema, result is commentary; only the separately
                // validated structured_output object is the review artifact.
                let response = event
                    .get("structured_output")
                    .filter(|response| response.is_object())
                    .ok_or(AgentRunError::InvalidStructuredOutput)?;
                self.finished = true;
                self.final_response = Some(response.to_string());
                log.append("Claude finished\n")?;
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ReviewConfig;
    use crate::prompt_builder;
    use crate::review::RoundId;
    use crate::{HeadSha, InputRevision, ObservedDiscussion, PrKey};
    use std::num::NonZeroU64;

    fn capture_stream(mut reader: impl Read, limit: u64, max_turns: u32) -> AgentRun {
        ClaudeAdapter.capture(&mut reader, limit, max_turns)
    }

    fn review_output() -> Value {
        serde_json::json!({
            "status": "ACCEPT_WITH_NITS",
            "comments": "A finding with \"quotes\".\n\nKeep these Markdown bytes.\r\n",
            "lead_advice": "Private advice for the lead."
        })
    }

    fn successful_result() -> Value {
        serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": "The review is complete.",
            "structured_output": review_output()
        })
    }

    fn test_config_json() -> serde_json::Value {
        serde_json::json!({
            "schema_version":7,
            "execution": crate::config::test_execution_json(),
            "models":[{"id":"model-alpha","coauthors":[]}],
            "agents":[{"id":"claude","models":["model-alpha"],"runner":crate::config::test_runner_json()}],
            "repositories":[{"name":"owner/repo", "prompt_file": crate::config::test_prompt_file("review.md"),"review_models":["model-alpha"],"runners":[{
                "agent":"claude","vm_archive":{"path":std::env::temp_dir().join("reviewer.tar"),"sha256":"a".repeat(64),"config_sha256":"c".repeat(64)}
            }]}]
        })
    }

    fn test_round(config: &ReviewConfig) -> RoundRecord {
        let key = PrKey::new("owner/repo", 1).unwrap();
        let choice = config.review_models_for(&key).unwrap();
        RoundRecord::new(
            RoundId::new("r-test").unwrap(),
            key,
            HeadSha::new("a".repeat(40)).unwrap(),
            InputRevision::new(1).unwrap(),
            ObservedDiscussion::new(NonZeroU64::new(1).unwrap(), vec![]).unwrap(),
            &choice[0],
            1,
        )
    }

    fn flag_value<'a>(command: &'a [OsString], flag: &str) -> &'a str {
        let index = command.iter().position(|arg| arg == flag).unwrap();
        command[index + 1].to_str().unwrap()
    }

    #[test]
    fn claude_permissions_match_trusted_tool_guidance() {
        let config = ReviewConfig::from_json(
            &serde_json::to_vec(&test_config_json()).unwrap(),
            std::path::Path::new("config.json"),
        )
        .unwrap();
        let round = test_round(&config);
        let command = command_arguments(
            "/usr/bin/claude",
            &round,
            "/sandbox/instructions/system-prompt.txt",
            "b",
        );
        assert_eq!(flag_value(&command, "--permission-mode"), "dontAsk");
        assert_eq!(flag_value(&command, "--tools"), "Read,Bash");
        assert_eq!(
            serde_json::from_str::<Value>(flag_value(&command, "--json-schema")).unwrap(),
            serde_json::from_str::<Value>(crate::review::OUTPUT_SCHEMA).unwrap()
        );

        let checkout = sandbox_layout::CHECKOUT_TARGET;
        // Pin the complete permission surface independently of command construction.
        let expected = format!(
            "Read(./src/**),Read(./instructions/**),Bash(git -C {checkout} diff *),Bash(git -C {checkout} log *),Bash(git -C {checkout} show *),Bash(git -C {checkout} status *),Bash(gh api --method GET *)"
        );
        let allowed = flag_value(&command, "--allowedTools");
        assert_eq!(allowed, expected);
        assert!(TOOL_GUIDANCE.contains(sandbox_layout::WORKDIR_TARGET));
        for path in ["./src/", "./instructions/"] {
            assert!(TOOL_GUIDANCE.contains(path), "missing guidance for {path}");
        }
        let granted_bash = allowed
            .split(',')
            .filter_map(|grant| grant.strip_prefix("Bash("))
            .map(|grant| format!("{} ...", grant.strip_suffix(" *)").unwrap()))
            .collect::<Vec<_>>();
        let documented_bash = TOOL_GUIDANCE
            .split_once("The runner grants these standalone Bash command forms: ")
            .unwrap()
            .1
            .split_once(". Run one allowed command per Bash call.")
            .unwrap()
            .0
            .replace(", and ", ", ")
            .split(", ")
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert_eq!(documented_bash, granted_bash);
        assert!(!TOOL_GUIDANCE.contains("Edit"));
    }

    #[test]
    fn captures_full_dialogue_and_fails_on_bad_utf8_or_limit() {
        let stream = format!(
            "{}\n{}\n{}\n",
            serde_json::json!({"type":"stream_event","event":{"delta":{"type":"text_delta","text":"hello"}}}),
            serde_json::json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"src/lib.rs"}}]}}),
            successful_result()
        );
        let capture = capture_stream(stream.as_bytes(), 1024, 100);
        assert!(capture.result.is_ok());
        assert_eq!(
            serde_json::from_str::<Value>(capture.final_response.as_deref().unwrap()).unwrap(),
            review_output()
        );
        assert!(capture.log.contains("hello"));
        assert!(capture.log.contains("src/lib.rs"));
        assert_eq!(
            capture_stream(stream.as_bytes(), 3, 100).result,
            Err(AgentRunError::LogTooLarge)
        );
        assert_eq!(
            capture_stream(&b"\xff\n"[..], 1024, 100).result,
            Err(AgentRunError::InvalidUtf8)
        );
    }

    #[test]
    fn rejects_streams_without_a_final_result_response() {
        let dialogue = b"{\"type\":\"assistant\",\"message\":{\"id\":\"msg-1\",\"content\":[]}}\n";
        assert_eq!(
            capture_stream(&dialogue[..], 1024, 10).result,
            Err(AgentRunError::InvalidEvent)
        );
        let mut missing_result = successful_result();
        missing_result
            .as_object_mut()
            .unwrap()
            .remove("structured_output");
        missing_result["result"] = review_output().to_string().into();
        assert_eq!(
            capture_stream(missing_result.to_string().as_bytes(), 1024, 10).result,
            Err(AgentRunError::InvalidStructuredOutput)
        );
    }

    #[test]
    fn captures_structured_review_instead_of_commentary_wrapped_json() {
        let mut event = successful_result();
        event["result"] = format!(
            "Explanatory preamble before the review.\n\n{}",
            review_output()
        )
        .into();
        let run = capture_stream(event.to_string().as_bytes(), 4096, 10);
        assert_eq!(run.result, Ok(()));

        let dir = tempfile::tempdir().unwrap();
        let artifact = dir.path().join("review-output.json");
        crate::review::write_agent_output(&artifact, run.final_response.as_deref().unwrap())
            .unwrap();
        let draft = crate::review::read_agent_output(&artifact, run.log).unwrap();
        assert_eq!(draft.status(), crate::review::ReviewStatus::AcceptWithNits);
        assert_eq!(
            draft.comments(),
            review_output()["comments"].as_str().unwrap()
        );
        assert_eq!(
            draft.lead_advice(),
            review_output()["lead_advice"].as_str().unwrap()
        );
    }

    #[test]
    fn rejects_nonobject_structured_output_without_falling_back_to_result_text() {
        for output in [
            Value::Null,
            serde_json::json!([]),
            serde_json::json!("{}"),
            serde_json::json!(true),
        ] {
            let mut event = successful_result();
            event["result"] = review_output().to_string().into();
            event["structured_output"] = output;
            let run = capture_stream(event.to_string().as_bytes(), 4096, 10);
            assert_eq!(run.result, Err(AgentRunError::InvalidStructuredOutput));
            assert!(run.final_response.is_none());
        }
    }

    #[test]
    fn failed_results_cannot_supply_a_structured_review() {
        for (subtype, is_error) in [
            ("error_max_structured_output_retries", true),
            ("error_during_execution", false),
            ("success", true),
        ] {
            let mut event = successful_result();
            event["subtype"] = subtype.into();
            event["is_error"] = is_error.into();
            let run = capture_stream(event.to_string().as_bytes(), 4096, 10);
            assert_eq!(run.result, Err(AgentRunError::Unsuccessful));
            assert!(run.final_response.is_none());
        }
    }

    #[test]
    fn repeated_assistant_events_with_one_message_id_count_as_one_turn() {
        let repeated = format!(
            "{}\n{}\n{}\n",
            serde_json::json!({"type":"assistant","message":{"id":"msg-1","content":[]}}),
            serde_json::json!({"type":"assistant","message":{"id":"msg-1","content":[]}}),
            successful_result()
        );
        assert!(capture_stream(repeated.as_bytes(), 1024, 1).result.is_ok());
        let distinct = b"{\"type\":\"assistant\",\"message\":{\"id\":\"msg-1\",\"content\":[]}}\n{\"type\":\"assistant\",\"message\":{\"id\":\"msg-2\",\"content\":[]}}\n";
        assert_eq!(
            capture_stream(distinct.as_slice(), 1024, 1).result,
            Err(AgentRunError::TooManyTurns)
        );
        let missing_ids = b"{\"type\":\"assistant\",\"message\":{\"content\":[]}}\n{\"type\":\"assistant\",\"message\":{\"content\":[]}}\n";
        assert_eq!(
            capture_stream(missing_ids.as_slice(), 1024, 1).result,
            Err(AgentRunError::TooManyTurns)
        );
    }

    #[test]
    fn trusted_review_rules_are_last_in_the_explicit_system_prompt() {
        let composed = prompt_builder::build(
            crate::config::AgentKind::Claude,
            "Configured review instructions.",
        );
        assert!(
            composed
                .text()
                .contains("Configured review instructions.\n\n")
        );
        assert!(composed.text().contains(TOOL_GUIDANCE));
        assert!(
            composed
                .text()
                .ends_with(include_str!("../prompts/system.md"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn timeout_keeps_emitted_dialogue_for_an_incomplete_private_log() {
        use crate::agent::{AgentInvocation, run};
        use crate::openshell::OpenShell;
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use std::sync::{Arc, atomic::AtomicBool};

        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("fake-openshell");
        fs::write(&executable, b"#!/bin/sh\nprintf '%s\\n' '{\"type\":\"stream_event\",\"event\":{\"delta\":{\"type\":\"text_delta\",\"text\":\"partial dialogue\"}}}'\nexec sleep 5\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let mut raw = test_config_json();
        raw["execution"]["openshell"] = executable.to_str().unwrap().into();
        raw["execution"]["timeout_seconds"] = 1.into();
        let config = ReviewConfig::from_json(
            &serde_json::to_vec(&raw).unwrap(),
            std::path::Path::new("config.json"),
        )
        .unwrap();
        let record = test_round(&config);
        let key = record.key();
        let choice = config.review_models_for(key).unwrap();
        let shell = OpenShell::new(config.execution());
        let prepared = ClaudeAdapter
            .prepare(
                &StagedArchive::for_test(
                    std::path::PathBuf::from("/tmp/image.tar"),
                    std::collections::BTreeMap::from([(
                        "CLAUDE_CODE_EXECUTABLE".into(),
                        "/usr/bin/claude".into(),
                    )]),
                ),
                choice[0].agent().runner(),
            )
            .unwrap();
        let result = run(
            &shell,
            config.execution(),
            choice[0].agent().runner(),
            &AgentInvocation::new(
                &prepared,
                &record,
                "/sandbox/instructions/system-prompt.txt",
                "b",
            ),
            Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(result.result, Err(AgentRunError::Timeout));
        assert!(result.log.contains("partial dialogue"));
    }

    #[cfg(unix)]
    #[test]
    fn successful_run_returns_the_structured_review() {
        use crate::agent::{AgentInvocation, run};
        use crate::openshell::OpenShell;
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;

        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("fake-openshell");
        fs::write(
            &executable,
            format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", successful_result()),
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let mut raw = test_config_json();
        raw["execution"]["openshell"] = executable.to_str().unwrap().into();
        let config = ReviewConfig::from_json(
            &serde_json::to_vec(&raw).unwrap(),
            std::path::Path::new("config.json"),
        )
        .unwrap();
        let record = test_round(&config);
        let key = record.key();
        let choice = config.review_models_for(key).unwrap();
        let shell = OpenShell::new(config.execution());
        let prepared = ClaudeAdapter
            .prepare(
                &StagedArchive::for_test(
                    std::path::PathBuf::from("/tmp/image.tar"),
                    std::collections::BTreeMap::from([(
                        "CLAUDE_CODE_EXECUTABLE".into(),
                        "/usr/bin/claude".into(),
                    )]),
                ),
                choice[0].agent().runner(),
            )
            .unwrap();
        let result = run(
            &shell,
            config.execution(),
            choice[0].agent().runner(),
            &AgentInvocation::new(
                &prepared,
                &record,
                "/sandbox/instructions/system-prompt.txt",
                "b",
            ),
            Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(result.result, Ok(()));
        assert_eq!(
            serde_json::from_str::<Value>(result.final_response.as_deref().unwrap()).unwrap(),
            review_output()
        );
    }
}
