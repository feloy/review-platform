//! Synchronous OpenShell 0.1.5 adapter for an operator-managed gateway.

use crate::archive::StagedArchive;
use crate::config::{AgentKind, AgentRunnerConfig, Driver, ExecutionConfig};
use crate::review::RoundRecord;
use crate::sandbox_layout;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OpenShellError {
    #[error("local VM review execution is unsupported on this platform")]
    PlatformUnsupported,
    #[error("podman review execution is unsupported; configure the vm driver")]
    PodmanUnsupported,
    #[error("OpenShell CLI must be version 0.1.5")]
    IncompatibleCli,
    #[error("running OpenShell {step}: {source}")]
    Launch {
        step: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("OpenShell {0} failed; check gateway authentication, connectivity, and operator logs")]
    Failed(&'static str),
    #[error("gateway does not report vm driver support")]
    VmUnavailable,
    #[error("required provider {0} is unavailable")]
    ProviderUnavailable(String),
    #[error("GitHub provider profile must enforce read-only REST access to api.github.com:443")]
    GithubProfileNotReadOnly,
    #[error("model provider profile is not inference-capable")]
    ModelProviderNotInference,
    #[error("provider profile {0} is unavailable")]
    ProfileUnavailable(String),
    #[error("image and agent environments both define {0}")]
    EnvironmentCollision(String),
    #[error("combined sandbox environment exceeds 64 KiB")]
    EnvironmentTooLarge,
    #[error("policy file {0} must be a regular file")]
    PolicyNotRegular(PathBuf),
    #[error("policy file checksum differs from configuration")]
    PolicyChanged,
    #[error("reading policy file {path}: {source}")]
    PolicyRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("storing private policy file {path}: {source}")]
    PolicyStore {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid gateway response for {0}")]
    InvalidResponse(&'static str),
}

pub struct OpenShell<'a> {
    config: &'a ExecutionConfig,
}

/// Host directories mounted into one review sandbox.
pub struct SandboxMounts<'a> {
    checkout: &'a Path,
    instructions: &'a Path,
    output: &'a Path,
}

impl<'a> SandboxMounts<'a> {
    pub fn new(checkout: &'a Path, instructions: &'a Path, output: &'a Path) -> Self {
        Self {
            checkout,
            instructions,
            output,
        }
    }
}

impl<'a> OpenShell<'a> {
    pub fn new(config: &'a ExecutionConfig) -> Self {
        Self { config }
    }

    pub fn preflight_gateway(&self) -> Result<(), OpenShellError> {
        if !cfg!(unix) {
            return Err(OpenShellError::PlatformUnsupported);
        }
        match self.config.driver() {
            Driver::Vm => {}
            Driver::Podman => return Err(OpenShellError::PodmanUnsupported),
        }
        let version = self.command(&["--version"], "version")?;
        if !version.status.success()
            || std::str::from_utf8(&version.stdout).ok().map(str::trim) != Some("openshell 0.1.5")
        {
            return Err(OpenShellError::IncompatibleCli);
        }
        self.require_success(&["whoami", "--output", "json"], "whoami")?;
        self.require_success(&["status", "--output", "json"], "status")?;
        let info =
            self.require_success(&["gateway", "info", "--output", "json"], "gateway info")?;
        let data: Value = serde_json::from_slice(&info.stdout)
            .map_err(|_| OpenShellError::InvalidResponse("gateway info"))?;
        if !reports_vm(&data) {
            return Err(OpenShellError::VmUnavailable);
        }
        let github = self.provider_profile(self.config.github_provider())?;
        if !github_profile_is_read_only(&github) {
            return Err(OpenShellError::GithubProfileNotReadOnly);
        }
        Ok(())
    }

    pub fn preflight_runner(&self, runner: &AgentRunnerConfig) -> Result<(), OpenShellError> {
        let model = self.provider_profile(runner.model_provider())?;
        if model.get("inference_capable") != Some(&Value::Bool(true)) {
            return Err(OpenShellError::ModelProviderNotInference);
        }
        self.verify_policy(runner)
    }

    fn provider_profile(&self, provider: &str) -> Result<Value, OpenShellError> {
        let details = self.command(&["provider", "get", provider], "provider get")?;
        if !details.status.success() {
            return Err(OpenShellError::ProviderUnavailable(provider.to_owned()));
        }
        let profile_type = provider_type(&details.stdout)
            .ok_or(OpenShellError::InvalidResponse("provider get"))?;
        let local = self.command(
            &["profile", "describe", "--output", "json", &profile_type],
            "profile describe",
        )?;
        let description = if local.status.success() {
            local
        } else {
            let global = self.command(
                &[
                    "profile",
                    "describe",
                    "--global",
                    "--output",
                    "json",
                    &profile_type,
                ],
                "profile describe",
            )?;
            if !global.status.success() {
                return Err(OpenShellError::ProfileUnavailable(profile_type));
            }
            global
        };
        serde_json::from_slice(&description.stdout)
            .map_err(|_| OpenShellError::InvalidResponse("profile describe"))
    }

    pub fn verify_policy(&self, runner: &AgentRunnerConfig) -> Result<(), OpenShellError> {
        let path = runner.policy();
        let metadata = fs::symlink_metadata(path).map_err(|source| OpenShellError::PolicyRead {
            path: path.to_owned(),
            source,
        })?;
        if !metadata.file_type().is_file() {
            return Err(OpenShellError::PolicyNotRegular(path.to_owned()));
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let mut file: File = options
            .open(path)
            .map_err(|source| OpenShellError::PolicyRead {
                path: path.to_owned(),
                source,
            })?;
        if !file
            .metadata()
            .map_err(|source| OpenShellError::PolicyRead {
                path: path.to_owned(),
                source,
            })?
            .file_type()
            .is_file()
        {
            return Err(OpenShellError::PolicyNotRegular(path.to_owned()));
        }
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 65536];
        loop {
            let size = file
                .read(&mut buffer)
                .map_err(|source| OpenShellError::PolicyRead {
                    path: path.to_owned(),
                    source,
                })?;
            if size == 0 {
                break;
            }
            digest.update(&buffer[..size]);
        }
        if format!("{:x}", digest.finalize()) != runner.policy_sha256().as_str() {
            return Err(OpenShellError::PolicyChanged);
        }
        Ok(())
    }

    /// Keep the exact verified policy bytes private for the sandbox creation call.
    pub fn stage_policy(
        &self,
        runner: &AgentRunnerConfig,
        destination: &Path,
    ) -> Result<(), OpenShellError> {
        self.verify_policy(runner)?;
        let mut source_options = OpenOptions::new();
        source_options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            source_options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            source_options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let mut source =
            source_options
                .open(runner.policy())
                .map_err(|source| OpenShellError::PolicyRead {
                    path: runner.policy().to_owned(),
                    source,
                })?;
        if !source
            .metadata()
            .map_err(|source| OpenShellError::PolicyRead {
                path: runner.policy().to_owned(),
                source,
            })?
            .file_type()
            .is_file()
        {
            return Err(OpenShellError::PolicyNotRegular(runner.policy().to_owned()));
        }
        let mut target_options = OpenOptions::new();
        target_options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            target_options.mode(0o600);
        }
        let mut target =
            target_options
                .open(destination)
                .map_err(|source| OpenShellError::PolicyStore {
                    path: destination.to_owned(),
                    source,
                })?;
        std::io::copy(&mut source, &mut target).map_err(|source| OpenShellError::PolicyStore {
            path: destination.to_owned(),
            source,
        })?;
        target
            .flush()
            .map_err(|source| OpenShellError::PolicyStore {
                path: destination.to_owned(),
                source,
            })?;
        target
            .sync_all()
            .map_err(|source| OpenShellError::PolicyStore {
                path: destination.to_owned(),
                source,
            })?;
        let staged_hash = {
            let mut file =
                File::open(destination).map_err(|source| OpenShellError::PolicyRead {
                    path: destination.to_owned(),
                    source,
                })?;
            let mut digest = Sha256::new();
            let mut buffer = [0_u8; 65536];
            loop {
                let size = file
                    .read(&mut buffer)
                    .map_err(|source| OpenShellError::PolicyRead {
                        path: destination.to_owned(),
                        source,
                    })?;
                if size == 0 {
                    break;
                }
                digest.update(&buffer[..size]);
            }
            format!("{:x}", digest.finalize())
        };
        if staged_hash != runner.policy_sha256().as_str() {
            return Err(OpenShellError::PolicyChanged);
        }
        Ok(())
    }

    pub fn create(
        &self,
        round: &RoundRecord,
        runner: &AgentRunnerConfig,
        image: &StagedArchive,
        policy: &Path,
        mounts: &SandboxMounts<'_>,
    ) -> Result<(), OpenShellError> {
        let args = self.create_args(round, runner, image, policy, mounts)?;
        let result = self.command_os(&args, "sandbox create")?;
        if !result.status.success() {
            return Err(OpenShellError::Failed("sandbox create"));
        }
        Ok(())
    }

    pub fn create_args(
        &self,
        round: &RoundRecord,
        runner: &AgentRunnerConfig,
        image: &StagedArchive,
        policy: &Path,
        mounts: &SandboxMounts<'_>,
    ) -> Result<Vec<OsString>, OpenShellError> {
        if self.config.driver() != Driver::Vm {
            return Err(OpenShellError::PodmanUnsupported);
        }
        let driver = serde_json::json!({"vm":{"mounts":[
            {"type":"bind","source":mounts.checkout,"target":sandbox_layout::CHECKOUT_TARGET,"read_only":true},
            {"type":"bind","source":mounts.instructions,"target":sandbox_layout::INSTRUCTIONS_TARGET,"read_only":true},
            {"type":"bind","source":mounts.output,"target":sandbox_layout::OUTPUT_TARGET,"read_only":false}
        ]}});
        let driver = serde_json::to_string(&driver)
            .map_err(|_| OpenShellError::InvalidResponse("mount configuration"))?;
        let mut args: Vec<OsString> = vec![
            "sandbox".into(),
            "create".into(),
            "--name".into(),
            round.sandbox().into(),
            "--from".into(),
            image.path().as_os_str().to_owned(),
            "--cpu".into(),
            self.config.cpu().into(),
            "--memory".into(),
            self.config.memory().into(),
            "--driver-config-json".into(),
            driver.into(),
            "--provider".into(),
            runner.model_provider().into(),
            "--provider".into(),
            self.config.github_provider().into(),
            "--policy".into(),
            policy.as_os_str().to_owned(),
            "--no-auto-providers".into(),
            "--no-tty".into(),
            "--detach".into(),
            "--restart-policy".into(),
            "never".into(),
        ];
        let mut environment = image.environment().clone();
        for (key, value) in runner.environment() {
            if environment.insert(key.clone(), value.clone()).is_some() {
                return Err(OpenShellError::EnvironmentCollision(key.clone()));
            }
        }
        match runner.kind() {
            AgentKind::Claude => {
                let key = "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY";
                if environment.insert(key.into(), "1".into()).is_some() {
                    return Err(OpenShellError::EnvironmentCollision(key.into()));
                }
            }
        }
        if environment
            .iter()
            .map(|(key, value)| key.len() + value.len() + 1)
            .sum::<usize>()
            > 64 * 1024
        {
            return Err(OpenShellError::EnvironmentTooLarge);
        }
        for (key, value) in &environment {
            args.push("--env".into());
            args.push(format!("{key}={value}").into());
        }
        args.push("--".into());
        args.extend(runner.hold_command().iter().map(OsString::from));
        Ok(args)
    }

    pub fn check_agent_executable(
        &self,
        round: &RoundRecord,
        executable: &str,
    ) -> Result<(), OpenShellError> {
        let result = self
            .exec_command(round, &[executable.into(), "--version".into()])
            .output()
            .map_err(|source| OpenShellError::Launch {
                step: "agent executable check",
                source,
            })?;
        if !result.status.success() {
            return Err(OpenShellError::Failed("agent executable check"));
        }
        Ok(())
    }

    pub fn stop(&self, round: &RoundRecord) -> Result<(), OpenShellError> {
        self.require_success_at(
            round.gateway(),
            round.workspace(),
            &["sandbox", "stop", round.sandbox()],
            "sandbox stop",
        )?;
        Ok(())
    }

    pub fn delete(&self, round: &RoundRecord) -> Result<(), OpenShellError> {
        let result = self.command_at(
            round.gateway(),
            round.workspace(),
            &["sandbox", "delete", round.sandbox()],
            "sandbox delete",
        )?;
        if result.status.success()
            || String::from_utf8_lossy(&result.stderr)
                .to_ascii_lowercase()
                .contains("not found")
        {
            Ok(())
        } else {
            Err(OpenShellError::Failed("sandbox delete"))
        }
    }

    pub fn exec_command(&self, round: &RoundRecord, command: &[OsString]) -> Command {
        self.exec_command_at(round, None, command)
    }

    pub fn exec_review_agent(&self, round: &RoundRecord, command: &[OsString]) -> Command {
        self.exec_command_at(round, Some(sandbox_layout::WORKDIR_TARGET), command)
    }

    fn exec_command_at(
        &self,
        round: &RoundRecord,
        workdir: Option<&str>,
        command: &[OsString],
    ) -> Command {
        let mut child = Command::new(self.config.openshell());
        self.base_args(&mut child, round.gateway(), round.workspace());
        child.args([
            "sandbox",
            "exec",
            round.sandbox(),
            "--no-tty",
            "--no-login-shell",
            "--timeout",
        ]);
        child.arg(self.config.timeout_seconds().to_string());
        if let Some(workdir) = workdir {
            child.args(["--workdir", workdir]);
        }
        child.arg("--");
        child.args(command);
        child
    }

    fn require_success(&self, args: &[&str], step: &'static str) -> Result<Output, OpenShellError> {
        self.require_success_at(self.config.gateway(), self.config.workspace(), args, step)
    }

    fn require_success_at(
        &self,
        gateway: &str,
        workspace: &str,
        args: &[&str],
        step: &'static str,
    ) -> Result<Output, OpenShellError> {
        let result = self.command_at(gateway, workspace, args, step)?;
        if !result.status.success() {
            return Err(OpenShellError::Failed(step));
        }
        Ok(result)
    }

    fn command(&self, args: &[&str], step: &'static str) -> Result<Output, OpenShellError> {
        self.command_at(self.config.gateway(), self.config.workspace(), args, step)
    }

    fn command_at(
        &self,
        gateway: &str,
        workspace: &str,
        args: &[&str],
        step: &'static str,
    ) -> Result<Output, OpenShellError> {
        let mut command = Command::new(self.config.openshell());
        self.base_args(&mut command, gateway, workspace);
        command.args(args);
        command
            .output()
            .map_err(|source| OpenShellError::Launch { step, source })
    }

    fn command_os(&self, args: &[OsString], step: &'static str) -> Result<Output, OpenShellError> {
        let mut command = Command::new(self.config.openshell());
        self.base_args(&mut command, self.config.gateway(), self.config.workspace());
        command.args(args);
        command
            .output()
            .map_err(|source| OpenShellError::Launch { step, source })
    }

    fn base_args(&self, command: &mut Command, gateway: &str, workspace: &str) {
        command.args([
            "--gateway",
            gateway,
            "--workspace",
            workspace,
            "--color",
            "never",
        ]);
        for key in [
            "OPENSHELL_GATEWAY",
            "OPENSHELL_GATEWAY_ENDPOINT",
            "OPENSHELL_GATEWAY_INSECURE",
            "OPENSHELL_WORKSPACE",
        ] {
            command.env_remove(key);
        }
    }
}

fn reports_vm(value: &Value) -> bool {
    value
        .get("compute_drivers")
        .and_then(Value::as_array)
        .is_some_and(|drivers| {
            drivers
                .iter()
                .any(|driver| driver.get("name").and_then(Value::as_str) == Some("vm"))
        })
}

fn provider_type(output: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(output).ok()?;
    let value = text
        .lines()
        .filter_map(|line| line.trim().split_once(':'))
        .find_map(|(key, value)| (key == "Type").then_some(value.trim()))?;
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return None;
    }
    Some(value.to_owned())
}

fn github_profile_is_read_only(profile: &Value) -> bool {
    profile.get("inference_capable") == Some(&Value::Bool(false))
        && profile
            .get("endpoints")
            .and_then(Value::as_array)
            .is_some_and(|endpoints| {
                !endpoints.is_empty()
                    && endpoints.iter().all(|endpoint| {
                        endpoint.get("host").and_then(Value::as_str) == Some("api.github.com")
                            && endpoint.get("port").and_then(Value::as_u64) == Some(443)
                            && endpoint.get("protocol").and_then(Value::as_str) == Some("rest")
                            && endpoint.get("access").and_then(Value::as_str) == Some("read-only")
                            && endpoint.get("enforcement").and_then(Value::as_str)
                                == Some("enforce")
                    })
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::config::ReviewConfig;
    #[cfg(unix)]
    use crate::review::{RoundId, RoundRecord};
    #[cfg(unix)]
    use crate::{HeadSha, InputRevision, ObservedDiscussion, PrKey};
    #[cfg(unix)]
    use std::num::NonZeroU64;
    #[test]
    fn driver_info_must_explicitly_report_vm() {
        assert!(reports_vm(
            &serde_json::json!({"compute_drivers":[{"name":"vm","capabilities":{"driver_name":"openshell-driver-vm"}}]})
        ));
        assert!(!reports_vm(
            &serde_json::json!({"compute_drivers":[{"name":"podman"}]})
        ));
        assert!(!reports_vm(&serde_json::json!({"status":"healthy"})));
        let readonly = serde_json::json!({"inference_capable":false,"endpoints":[{
            "host":"api.github.com","port":443,"protocol":"rest","access":"read-only","enforcement":"enforce"
        }]});
        assert!(github_profile_is_read_only(&readonly));
        let mut writable = readonly;
        writable["endpoints"][0]["access"] = "read-write".into();
        assert!(!github_profile_is_read_only(&writable));
        assert_eq!(
            provider_type(b"Provider:\n  Type: github-pr-readonly\n"),
            Some("github-pr-readonly".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn fake_cli_preflight_and_command_use_explicit_gateway_providers_and_read_only_mounts() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("openshell");
        let calls = dir.path().join("calls.txt");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n  *--version*) echo 'openshell 0.1.5';;\n  *'gateway info'*) echo '{{\"compute_drivers\":[{{\"name\":\"vm\"}}]}}';;\n  *'provider get'*) for last do :; done; echo \"Type: $last\";;\n  *test-model-provider*) echo '{{\"inference_capable\":true}}';;\n  *test-github-provider*) echo '{{\"inference_capable\":false,\"endpoints\":[{{\"host\":\"api.github.com\",\"port\":443,\"protocol\":\"rest\",\"access\":\"read-only\",\"enforcement\":\"enforce\"}}]}}';;\nesac\n",
            calls.display()
        );
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let policy = dir.path().join("policy.yaml");
        fs::write(&policy, b"policy: test\n").unwrap();
        let mut raw = serde_json::json!({
            "schema_version":5,
            "execution": crate::config::test_execution_json(),
            "models":[{"id":"model-alpha","coauthors":[]}],
            "agents":[{"id":"claude","models":["model-alpha"],"prompt":"Review.","runner":crate::config::test_runner_json()}],
            "repositories":[{"name":"owner/repo","review_models":["model-alpha"],"runners":[{
                "agent":"claude","vm_archive":{"path":"/tmp/reviewer.tar","sha256":"a".repeat(64),"config_sha256":"c".repeat(64)}
            }]}]
        });
        raw["execution"]["openshell"] = executable.to_str().unwrap().into();
        raw["agents"][0]["runner"]["policy"] = policy.to_str().unwrap().into();
        raw["agents"][0]["runner"]["policy_sha256"] =
            format!("{:x}", Sha256::digest(b"policy: test\n")).into();
        let config = ReviewConfig::from_json(&serde_json::to_vec(&raw).unwrap()).unwrap();
        let shell = OpenShell::new(config.execution());
        shell.preflight_gateway().unwrap();
        shell.preflight_runner(config.agents()[0].runner()).unwrap();
        let key = PrKey::new("owner/repo", 42).unwrap();
        let choice = config.review_models_for(&key).unwrap();
        let record = RoundRecord::new(
            RoundId::new("r-test").unwrap(),
            key,
            HeadSha::new("a".repeat(40)).unwrap(),
            InputRevision::new(2).unwrap(),
            ObservedDiscussion::new(NonZeroU64::new(7).unwrap(), vec![]).unwrap(),
            &choice[0],
            1,
        );
        let review_command = shell.exec_review_agent(&record, &[OsString::from("claude")]);
        let review_args = review_command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(
            review_args.windows(2).any(|args| {
                args[0] == "--workdir" && args[1] == sandbox_layout::WORKDIR_TARGET
            })
        );
        let args = shell
            .create_args(
                &record,
                choice[0].agent().runner(),
                &StagedArchive::for_test(
                    PathBuf::from("/tmp/archive.tar"),
                    std::collections::BTreeMap::from([
                        ("PATH".into(), "/usr/local/bin:/usr/bin".into()),
                        (
                            "CLAUDE_CODE_EXECUTABLE".into(),
                            "/usr/local/bin/claude".into(),
                        ),
                    ]),
                ),
                &policy,
                &SandboxMounts::new(
                    Path::new("/tmp/src"),
                    Path::new("/tmp/instructions"),
                    Path::new("/tmp/output"),
                ),
            )
            .unwrap();
        let args: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--no-auto-providers".to_owned()));
        assert!(!args.contains(&"--auto-providers".to_owned()));
        assert_eq!(
            args.iter()
                .filter(|arg| arg.as_str() == "--provider")
                .count(),
            2
        );
        assert!(args.windows(2).any(|items| {
            items[0] == "--env" && items[1] == "CLAUDE_CODE_EXECUTABLE=/usr/local/bin/claude"
        }));
        assert!(
            args.windows(2)
                .any(|items| { items[0] == "--env" && items[1] == "PATH=/usr/local/bin:/usr/bin" })
        );
        assert!(args.windows(2).any(|items| {
            items[0] == "--env" && items[1] == "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1"
        }));
        assert!(
            args.windows(2)
                .any(|items| { items[0] == "--provider" && items[1] == "test-model-provider" })
        );
        let mount_json = args
            .get(
                args.iter()
                    .position(|arg| arg == "--driver-config-json")
                    .unwrap()
                    + 1,
            )
            .unwrap();
        let mounts: Value = serde_json::from_str(mount_json).unwrap();
        assert_eq!(mounts["vm"]["mounts"][0]["read_only"], true);
        assert_eq!(mounts["vm"]["mounts"][1]["read_only"], true);
        assert_eq!(mounts["vm"]["mounts"][2]["read_only"], false);
        assert_eq!(
            mounts["vm"]["mounts"][0]["target"],
            sandbox_layout::CHECKOUT_TARGET
        );
        assert_eq!(
            mounts["vm"]["mounts"][1]["target"],
            sandbox_layout::INSTRUCTIONS_TARGET
        );
        assert_eq!(
            mounts["vm"]["mounts"][2]["target"],
            sandbox_layout::OUTPUT_TARGET
        );
        let mut conflicting = raw.clone();
        conflicting["agents"][0]["runner"]["environment"] = serde_json::json!({"PATH":"/bin"});
        let conflicting =
            ReviewConfig::from_json(&serde_json::to_vec(&conflicting).unwrap()).unwrap();
        let conflicting_choice = conflicting.review_models_for(record.key()).unwrap();
        assert!(matches!(
            OpenShell::new(conflicting.execution()).create_args(
                &record,
                conflicting_choice[0].agent().runner(),
                &StagedArchive::for_test(
                    PathBuf::from("/tmp/archive.tar"),
                    std::collections::BTreeMap::from([("PATH".into(), "/usr/bin".into())]),
                ),
                &policy,
                &SandboxMounts::new(
                    Path::new("/tmp/src"),
                    Path::new("/tmp/instructions"),
                    Path::new("/tmp/output"),
                ),
            ),
            Err(OpenShellError::EnvironmentCollision(key)) if key == "PATH"
        ));
        let calls = fs::read_to_string(&calls).unwrap();
        for line in calls.lines() {
            assert!(line.contains("--gateway test-gateway --workspace default"));
        }
        raw["execution"]["driver"] = "podman".into();
        let podman = ReviewConfig::from_json(&serde_json::to_vec(&raw).unwrap()).unwrap();
        let before = fs::read(dir.path().join("calls.txt")).unwrap();
        assert!(matches!(
            OpenShell::new(podman.execution()).preflight_gateway(),
            Err(OpenShellError::PodmanUnsupported)
        ));
        assert_eq!(fs::read(dir.path().join("calls.txt")).unwrap(), before);
        raw["execution"]["driver"] = "unknown".into();
        assert!(ReviewConfig::from_json(&serde_json::to_vec(&raw).unwrap()).is_err());
    }
}
