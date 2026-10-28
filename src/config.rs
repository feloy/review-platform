//! Validated model choices, compatible agent runners, and VM archives.

use crate::{PrKey, StateError, sandbox_layout};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

const MAX_PROMPT_BYTES: usize = 64 * 1024;
const MAX_COAUTHOR_NAME_BYTES: usize = 128;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("invalid configuration JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported configuration schema version: {0}")]
    UnsupportedSchema(u32),
    #[error(
        "invalid agent ID {0:?}; use 1-64 lowercase ASCII letters, digits, hyphens, or underscores, starting with a letter"
    )]
    InvalidAgentId(String),
    #[error(
        "invalid model ID {0:?}; use 1-128 lowercase ASCII letters, digits, dots, slashes, colons, hyphens, or underscores, starting with a lowercase letter"
    )]
    InvalidModelId(String),
    #[error("co-author model name must be 1-128 bytes without control characters: {0:?}")]
    InvalidCoauthorName(String),
    #[error("invalid co-author email {0:?}")]
    InvalidCoauthorEmail(String),
    #[error(
        "VM archive path must be an absolute .tar path without control characters or traversal: {0:?}"
    )]
    InvalidArchivePath(PathBuf),
    #[error("SHA-256 must contain 64 hexadecimal characters")]
    InvalidArchiveChecksum,
    #[error("agent {0} has an empty or oversized review prompt (maximum 64 KiB)")]
    InvalidPrompt(AgentId),
    #[error("prompt SHA-256 must contain 64 hexadecimal characters")]
    InvalidPromptHash,
    #[error("duplicate agent ID {0}")]
    DuplicateAgent(AgentId),
    #[error("duplicate model ID {0}")]
    DuplicateModel(ModelId),
    #[error("co-author signature {name:?} <{email}> is assigned to both {first} and {second}")]
    DuplicateCoauthorIdentity {
        name: String,
        email: String,
        first: ModelId,
        second: ModelId,
    },
    #[error("agent {0} has no supported models")]
    NoSupportedModels(AgentId),
    #[error("agent {agent} lists model {model} more than once")]
    DuplicateSupportedModel { agent: AgentId, model: ModelId },
    #[error("agent {agent} refers to unknown model {model}")]
    UnknownAgentModel { agent: AgentId, model: ModelId },
    #[error("invalid configured repository: {0}")]
    InvalidRepository(#[source] StateError),
    #[error("duplicate configured repository {0}")]
    DuplicateRepository(String),
    #[error("repository {0} has no review models")]
    NoReviewModels(String),
    #[error("repository {0} has no agent runners")]
    NoRunners(String),
    #[error("repository {repository} refers to unknown model {model}")]
    UnknownReviewModel { repository: String, model: ModelId },
    #[error("repository {repository} lists model {model} more than once")]
    DuplicateReviewModel { repository: String, model: ModelId },
    #[error("repository {repository} refers to unknown agent {agent}")]
    UnknownAgent { repository: String, agent: AgentId },
    #[error("repository {repository} configures agent runner {agent} more than once")]
    DuplicateRunner { repository: String, agent: AgentId },
    #[error("repository {repository} has no agent runner supporting model {model}")]
    NoCompatibleAgent { repository: String, model: ModelId },
    #[error("repository {0} is not configured")]
    RepositoryNotConfigured(String),
    #[error("model {model} is not configured for review in {repository}")]
    ModelNotConfigured { repository: String, model: ModelId },
    #[error("invalid execution configuration: {0}")]
    InvalidExecution(String),
    #[error("invalid agent runner configuration: {0}")]
    InvalidRunner(String),
    #[error("agent runner kind {0:?} is unsupported; expected claude")]
    InvalidRunnerKind(String),
    #[error("driver {0:?} is invalid; expected vm or podman")]
    InvalidDriver(String),
}

/// Stable configured agent identifier, independent of GitHub commit text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct AgentId(String);

impl AgentId {
    pub fn new(value: impl Into<String>) -> Result<Self, ConfigError> {
        let value = value.into();
        let valid = (1..=64).contains(&value.len())
            && value.as_bytes()[0].is_ascii_lowercase()
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
            });
        if !valid {
            return Err(ConfigError::InvalidAgentId(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AgentId {
    type Error = ConfigError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Exact provider model identifier used for review selection.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct ModelId(String);

impl ModelId {
    pub fn new(value: impl Into<String>) -> Result<Self, ConfigError> {
        let value = value.into();
        let valid = (1..=128).contains(&value.len())
            && value.as_bytes()[0].is_ascii_lowercase()
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'/' | b':' | b'-' | b'_')
            });
        if !valid {
            return Err(ConfigError::InvalidModelId(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ModelId {
    type Error = ConfigError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Normalized commit trailer name and email; an author-controlled model hint.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawCoauthorIdentity")]
pub struct CoauthorIdentity {
    name: String,
    email: String,
}

impl CoauthorIdentity {
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Result<Self, ConfigError> {
        let name = name.into().trim().to_lowercase();
        if name.is_empty()
            || name.len() > MAX_COAUTHOR_NAME_BYTES
            || name.chars().any(char::is_control)
        {
            return Err(ConfigError::InvalidCoauthorName(name));
        }
        Ok(Self {
            name,
            email: normalize_email(email.into())?,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn email(&self) -> &str {
        &self.email
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCoauthorIdentity {
    name: String,
    email: String,
}

impl TryFrom<RawCoauthorIdentity> for CoauthorIdentity {
    type Error = ConfigError;

    fn try_from(value: RawCoauthorIdentity) -> Result<Self, Self::Error> {
        Self::new(value.name, value.email)
    }
}

/// SHA-256 of the configured VM archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct ArchiveHash(String);

impl ArchiveHash {
    pub fn new(value: impl Into<String>) -> Result<Self, ConfigError> {
        let value = value.into();
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ConfigError::InvalidArchiveChecksum);
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ArchiveHash {
    type Error = ConfigError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl fmt::Display for ArchiveHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A local VM rootfs tar and its separate OCI configuration sidecar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawVmArchive")]
pub struct VmArchive {
    path: PathBuf,
    sha256: ArchiveHash,
    config_sha256: ArchiveHash,
}

impl VmArchive {
    pub fn new(
        path: PathBuf,
        sha256: ArchiveHash,
        config_sha256: ArchiveHash,
    ) -> Result<Self, ConfigError> {
        let valid = path.is_absolute()
            && path.extension().is_some_and(|extension| extension == "tar")
            && !path
                .components()
                .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
            && path.to_str().is_some_and(|value| {
                !value.chars().any(|character| {
                    character.is_control()
                        || matches!(
                            character,
                            '\u{061c}'
                                | '\u{200e}'
                                | '\u{200f}'
                                | '\u{202a}'..='\u{202e}'
                                | '\u{2066}'..='\u{2069}'
                        )
                })
            });
        if !valid {
            return Err(ConfigError::InvalidArchivePath(path));
        }
        Ok(Self {
            path,
            sha256,
            config_sha256,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn checksum(&self) -> &ArchiveHash {
        &self.sha256
    }

    pub fn config_path(&self) -> PathBuf {
        let mut name = self.path.as_os_str().to_os_string();
        name.push(".config.json");
        PathBuf::from(name)
    }

    pub fn config_checksum(&self) -> &ArchiveHash {
        &self.config_sha256
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawVmArchive {
    path: PathBuf,
    sha256: String,
    config_sha256: String,
}

impl TryFrom<RawVmArchive> for VmArchive {
    type Error = ConfigError;

    fn try_from(value: RawVmArchive) -> Result<Self, Self::Error> {
        Self::new(
            value.path,
            ArchiveHash::new(value.sha256)?,
            ArchiveHash::new(value.config_sha256)?,
        )
    }
}

/// SHA-256 of the exact configured prompt, without storing it in state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct PromptHash(String);

impl PromptHash {
    pub fn new(value: impl Into<String>) -> Result<Self, ConfigError> {
        let value = value.into();
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ConfigError::InvalidPromptHash);
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    pub(crate) fn of(prompt: &str) -> Self {
        let digest = Sha256::digest(prompt.as_bytes());
        Self(format!("{digest:x}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for PromptHash {
    type Error = ConfigError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl fmt::Display for PromptHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug)]
pub struct ReviewConfig {
    models: Vec<Model>,
    agents: Vec<Agent>,
    repositories: Vec<RepositoryConfig>,
    execution: ExecutionConfig,
}

/// Trusted, non-secret OpenShell and Claude execution settings.
#[derive(Debug)]
pub struct ExecutionConfig {
    openshell: PathBuf,
    gateway: String,
    workspace: String,
    driver: Driver,
    github_provider: String,
    timeout_seconds: u64,
    cpu: String,
    memory: String,
    log_limit_bytes: u64,
    hash: PromptHash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Driver {
    Vm,
    Podman,
}

impl Driver {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vm => "vm",
            Self::Podman => "podman",
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawExecution {
    openshell: PathBuf,
    gateway: String,
    workspace: String,
    driver: String,
    github_provider: String,
    #[serde(default = "default_timeout")]
    timeout_seconds: u64,
    #[serde(default = "default_cpu")]
    cpu: String,
    #[serde(default = "default_memory")]
    memory: String,
    #[serde(default = "default_log_limit")]
    log_limit_bytes: u64,
}

fn default_timeout() -> u64 {
    1800
}
fn default_cpu() -> String {
    "2".to_owned()
}
fn default_memory() -> String {
    "4Gi".to_owned()
}
fn default_log_limit() -> u64 {
    8 * 1024 * 1024
}

impl TryFrom<RawExecution> for ExecutionConfig {
    type Error = ConfigError;

    fn try_from(raw: RawExecution) -> Result<Self, Self::Error> {
        let hash = PromptHash::of(&serde_json::to_string(&serde_json::json!({
            "execution": &raw,
            "mount_targets": [
                sandbox_layout::CHECKOUT_TARGET,
                sandbox_layout::INSTRUCTIONS_TARGET,
                sandbox_layout::OUTPUT_TARGET,
            ]
        }))?);
        let driver = match raw.driver.as_str() {
            "vm" => Driver::Vm,
            "podman" => Driver::Podman,
            _ => return Err(ConfigError::InvalidDriver(raw.driver)),
        };
        for value in [&raw.gateway, &raw.workspace, &raw.github_provider] {
            if !valid_name(value) {
                return Err(ConfigError::InvalidExecution(
                    "gateway, workspace, and GitHub provider names must be simple ASCII names"
                        .into(),
                ));
            }
        }
        if !raw.openshell.is_absolute()
            || raw
                .openshell
                .components()
                .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
        {
            return Err(ConfigError::InvalidExecution(
                "OpenShell path must be absolute without traversal".into(),
            ));
        }
        if raw.timeout_seconds == 0
            || raw.timeout_seconds > 7200
            || raw.log_limit_bytes == 0
            || raw.log_limit_bytes > 64 * 1024 * 1024
            || !raw
                .cpu
                .parse::<f64>()
                .ok()
                .is_some_and(|cpu| cpu.is_finite() && cpu > 0.0 && cpu <= 64.0)
            || !valid_memory(&raw.memory)
        {
            return Err(ConfigError::InvalidExecution(
                "invalid lifecycle or resource limits".into(),
            ));
        }
        Ok(Self {
            openshell: raw.openshell,
            gateway: raw.gateway,
            workspace: raw.workspace,
            driver,
            github_provider: raw.github_provider,
            timeout_seconds: raw.timeout_seconds,
            cpu: raw.cpu,
            memory: raw.memory,
            log_limit_bytes: raw.log_limit_bytes,
            hash,
        })
    }
}

fn valid_memory(value: &str) -> bool {
    for suffix in ["Mi", "Gi", "M", "G"] {
        if let Some(amount) = value.strip_suffix(suffix) {
            return amount
                .parse::<u64>()
                .ok()
                .is_some_and(|amount| amount > 0 && amount <= 65536);
        }
    }
    false
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
}

/// Executable paths inside the Linux VM use POSIX syntax on every host platform.
pub(crate) fn valid_vm_absolute_path(value: &str) -> bool {
    value.strip_prefix('/').is_some_and(|rest| {
        !rest.is_empty()
            && rest.split('/').all(|component| {
                !component.is_empty()
                    && component != "."
                    && component != ".."
                    && !component.chars().any(char::is_control)
            })
    })
}

pub(crate) fn valid_environment_entry(key: &str, value: &str) -> bool {
    let uppercase = key.to_ascii_uppercase();
    !key.is_empty()
        && key.len() <= 128
        && (key.as_bytes()[0].is_ascii_alphabetic() || key.as_bytes()[0] == b'_')
        && key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
        && !uppercase.contains("TOKEN")
        && !uppercase.contains("SECRET")
        && !uppercase.contains("PASSWORD")
        && !uppercase.ends_with("KEY")
        && value.len() <= 8192
        && !value.chars().any(char::is_control)
}

impl ExecutionConfig {
    pub fn openshell(&self) -> &Path {
        &self.openshell
    }
    pub fn gateway(&self) -> &str {
        &self.gateway
    }
    pub fn workspace(&self) -> &str {
        &self.workspace
    }
    pub fn driver(&self) -> Driver {
        self.driver
    }
    pub fn github_provider(&self) -> &str {
        &self.github_provider
    }
    pub fn timeout_seconds(&self) -> u64 {
        self.timeout_seconds
    }
    pub fn cpu(&self) -> &str {
        &self.cpu
    }
    pub fn memory(&self) -> &str {
        &self.memory
    }
    pub fn log_limit_bytes(&self) -> u64 {
        self.log_limit_bytes
    }
    pub fn hash(&self) -> &PromptHash {
        &self.hash
    }
}

/// An agent adapter supported by this version of the review runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    Claude,
}

impl AgentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
        }
    }
}

/// Trusted settings used only after the selected model resolves to this agent.
#[derive(Debug)]
pub struct AgentRunnerConfig {
    kind: AgentKind,
    model_provider: String,
    environment: BTreeMap<String, String>,
    policy: PathBuf,
    policy_sha256: ArchiveHash,
    hold_command: Vec<String>,
    max_turns: u32,
    hash: PromptHash,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawAgentRunner {
    kind: String,
    model_provider: String,
    #[serde(default)]
    environment: BTreeMap<String, String>,
    policy: PathBuf,
    policy_sha256: String,
    hold_command: Vec<String>,
    #[serde(default = "default_turns")]
    max_turns: u32,
}

fn default_turns() -> u32 {
    100
}

impl TryFrom<RawAgentRunner> for AgentRunnerConfig {
    type Error = ConfigError;

    fn try_from(raw: RawAgentRunner) -> Result<Self, Self::Error> {
        let hash = PromptHash::of(&serde_json::to_string(&raw)?);
        let kind = match raw.kind.as_str() {
            "claude" => AgentKind::Claude,
            _ => return Err(ConfigError::InvalidRunnerKind(raw.kind)),
        };
        if !valid_name(&raw.model_provider) {
            return Err(ConfigError::InvalidRunner(
                "model provider must be a simple ASCII name".into(),
            ));
        }
        if !raw.policy.is_absolute()
            || raw
                .policy
                .components()
                .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
        {
            return Err(ConfigError::InvalidRunner(
                "policy path must be absolute without traversal".into(),
            ));
        }
        if raw.hold_command.is_empty()
            || !valid_vm_absolute_path(&raw.hold_command[0])
            || raw.hold_command.iter().any(|value| {
                value.is_empty() || value.len() > 32768 || value.chars().any(char::is_control)
            })
        {
            return Err(ConfigError::InvalidRunner(
                "hold command must start with an absolute VM executable and contain bounded single-line arguments".into(),
            ));
        }
        if raw.environment.len() > 256
            || raw
                .environment
                .iter()
                .map(|(key, value)| key.len() + value.len() + 1)
                .sum::<usize>()
                > 64 * 1024
            || raw
                .environment
                .iter()
                .any(|(key, value)| !valid_environment_entry(key, value))
            || raw.environment.contains_key("CLAUDE_CODE_EXECUTABLE")
            || raw
                .environment
                .contains_key("CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY")
        {
            return Err(ConfigError::InvalidRunner(
                "environment must contain bounded, non-secret values and cannot replace image or mandatory Claude settings".into(),
            ));
        }
        if raw.max_turns == 0 || raw.max_turns > 1000 {
            return Err(ConfigError::InvalidRunner(
                "max_turns must be between 1 and 1000".into(),
            ));
        }
        Ok(Self {
            kind,
            model_provider: raw.model_provider,
            environment: raw.environment,
            policy: raw.policy,
            policy_sha256: ArchiveHash::new(raw.policy_sha256)?,
            hold_command: raw.hold_command,
            max_turns: raw.max_turns,
            hash,
        })
    }
}

impl AgentRunnerConfig {
    pub fn kind(&self) -> AgentKind {
        self.kind
    }
    pub fn model_provider(&self) -> &str {
        &self.model_provider
    }
    pub fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }
    pub fn policy(&self) -> &Path {
        &self.policy
    }
    pub fn policy_sha256(&self) -> &ArchiveHash {
        &self.policy_sha256
    }
    pub fn hold_command(&self) -> &[String] {
        &self.hold_command
    }
    pub fn max_turns(&self) -> u32 {
        self.max_turns
    }
    pub fn hash(&self) -> &PromptHash {
        &self.hash
    }
}

#[derive(Debug)]
pub struct Model {
    id: ModelId,
    coauthors: Vec<CoauthorIdentity>,
}

impl Model {
    pub fn id(&self) -> &ModelId {
        &self.id
    }

    pub fn matches_coauthor(&self, coauthors: &HashSet<CoauthorIdentity>) -> bool {
        self.coauthors
            .iter()
            .any(|identity| coauthors.contains(identity))
    }
}

#[derive(Debug)]
pub struct Agent {
    id: AgentId,
    models: Vec<ModelId>,
    prompt: String,
    prompt_hash: PromptHash,
    runner: AgentRunnerConfig,
}

impl Agent {
    pub fn id(&self) -> &AgentId {
        &self.id
    }

    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    pub fn prompt_hash(&self) -> &PromptHash {
        &self.prompt_hash
    }

    pub fn runner(&self) -> &AgentRunnerConfig {
        &self.runner
    }

    pub fn supports(&self, model: &ModelId) -> bool {
        self.models.contains(model)
    }
}

#[derive(Debug)]
pub struct RepositoryConfig {
    name: String,
    review_models: Vec<ModelId>,
    runners: Vec<Runner>,
}

#[derive(Debug)]
struct Runner {
    agent: AgentId,
    archive: VmArchive,
}

pub struct ResolvedReviewModel<'a> {
    repository: &'a str,
    model: &'a Model,
    agent: &'a Agent,
    archive: &'a VmArchive,
    execution: &'a ExecutionConfig,
}

impl<'a> ResolvedReviewModel<'a> {
    pub fn repository(&self) -> &'a str {
        self.repository
    }

    pub fn model(&self) -> &'a Model {
        self.model
    }

    pub fn agent(&self) -> &'a Agent {
        self.agent
    }

    pub fn archive(&self) -> &'a VmArchive {
        self.archive
    }
    pub fn execution(&self) -> &'a ExecutionConfig {
        self.execution
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(rename = "schema_version")]
    _schema_version: u32,
    models: Vec<RawModel>,
    agents: Vec<RawAgent>,
    repositories: Vec<RawRepository>,
    execution: RawExecution,
}

#[derive(Deserialize)]
struct SchemaHeader {
    schema_version: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawModel {
    id: String,
    coauthors: Vec<RawCoauthorIdentity>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAgent {
    id: String,
    models: Vec<String>,
    prompt: String,
    runner: RawAgentRunner,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRepository {
    name: String,
    review_models: Vec<String>,
    runners: Vec<RawRunner>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRunner {
    agent: String,
    vm_archive: RawVmArchive,
}

impl ReviewConfig {
    pub fn execution(&self) -> &ExecutionConfig {
        &self.execution
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self, ConfigError> {
        let header: SchemaHeader = serde_json::from_slice(bytes)?;
        if header.schema_version != 5 {
            return Err(ConfigError::UnsupportedSchema(header.schema_version));
        }
        let raw: RawConfig = serde_json::from_slice(bytes)?;
        let mut models = Vec::with_capacity(raw.models.len());
        let mut seen_models = HashSet::new();
        let mut seen_coauthors = HashMap::new();
        for raw_model in raw.models {
            let id = ModelId::new(raw_model.id)?;
            if !seen_models.insert(id.clone()) {
                return Err(ConfigError::DuplicateModel(id));
            }
            let mut coauthors = Vec::new();
            for raw_coauthor in raw_model.coauthors {
                let identity = CoauthorIdentity::try_from(raw_coauthor)?;
                if let Some(first) = seen_coauthors.insert(identity.clone(), id.clone()) {
                    return Err(ConfigError::DuplicateCoauthorIdentity {
                        name: identity.name().to_owned(),
                        email: identity.email().to_owned(),
                        first,
                        second: id,
                    });
                }
                coauthors.push(identity);
            }
            models.push(Model { id, coauthors });
        }
        let mut seen_agents = HashSet::new();
        let mut agents = Vec::with_capacity(raw.agents.len());
        for raw_agent in raw.agents {
            let id = AgentId::new(raw_agent.id)?;
            if !seen_agents.insert(id.clone()) {
                return Err(ConfigError::DuplicateAgent(id));
            }
            if raw_agent.prompt.trim().is_empty() || raw_agent.prompt.len() > MAX_PROMPT_BYTES {
                return Err(ConfigError::InvalidPrompt(id));
            }
            if raw_agent.models.is_empty() {
                return Err(ConfigError::NoSupportedModels(id));
            }
            let mut seen_supported_models = HashSet::new();
            let mut supported_models = Vec::new();
            for raw_model in raw_agent.models {
                let model = ModelId::new(raw_model)?;
                if !seen_models.contains(&model) {
                    return Err(ConfigError::UnknownAgentModel { agent: id, model });
                }
                if !seen_supported_models.insert(model.clone()) {
                    return Err(ConfigError::DuplicateSupportedModel { agent: id, model });
                }
                supported_models.push(model);
            }
            agents.push(Agent {
                id,
                models: supported_models,
                prompt_hash: PromptHash::of(&raw_agent.prompt),
                prompt: raw_agent.prompt,
                runner: AgentRunnerConfig::try_from(raw_agent.runner)?,
            });
        }
        let mut seen_repositories = HashSet::new();
        let mut repositories = Vec::with_capacity(raw.repositories.len());
        for raw_repository in raw.repositories {
            let name = PrKey::new(raw_repository.name, 1)
                .map_err(ConfigError::InvalidRepository)?
                .repository()
                .to_owned();
            if !seen_repositories.insert(name.clone()) {
                return Err(ConfigError::DuplicateRepository(name));
            }
            if raw_repository.review_models.is_empty() {
                return Err(ConfigError::NoReviewModels(name));
            }
            if raw_repository.runners.is_empty() {
                return Err(ConfigError::NoRunners(name));
            }
            let mut seen_review_models = HashSet::new();
            let mut review_models = Vec::new();
            for raw_model in raw_repository.review_models {
                let model = ModelId::new(raw_model)?;
                if !seen_models.contains(&model) {
                    return Err(ConfigError::UnknownReviewModel {
                        repository: name,
                        model,
                    });
                }
                if !seen_review_models.insert(model.clone()) {
                    return Err(ConfigError::DuplicateReviewModel {
                        repository: name,
                        model,
                    });
                }
                review_models.push(model);
            }
            let mut seen_runners = HashSet::new();
            let mut runners = Vec::new();
            for raw_runner in raw_repository.runners {
                let agent = AgentId::new(raw_runner.agent)?;
                let archive = VmArchive::try_from(raw_runner.vm_archive)?;
                if !seen_agents.contains(&agent) {
                    return Err(ConfigError::UnknownAgent {
                        repository: name,
                        agent,
                    });
                }
                if !seen_runners.insert(agent.clone()) {
                    return Err(ConfigError::DuplicateRunner {
                        repository: name,
                        agent,
                    });
                }
                runners.push(Runner { agent, archive });
            }
            repositories.push(RepositoryConfig {
                name,
                review_models,
                runners,
            });
        }
        let config = Self {
            models,
            agents,
            repositories,
            execution: ExecutionConfig::try_from(raw.execution)?,
        };
        for agent in &config.agents {
            if agent.runner.model_provider() == config.execution.github_provider() {
                return Err(ConfigError::InvalidRunner(format!(
                    "agent {} uses the GitHub provider as its model provider",
                    agent.id
                )));
            }
        }
        for repository in &config.repositories {
            let key =
                PrKey::new(repository.name.clone(), 1).map_err(ConfigError::InvalidRepository)?;
            config.review_models_for(&key)?;
        }
        Ok(config)
    }

    pub fn review_models_for(
        &self,
        key: &PrKey,
    ) -> Result<Vec<ResolvedReviewModel<'_>>, ConfigError> {
        let repository = self
            .repositories
            .iter()
            .find(|repository| repository.name == key.repository())
            .ok_or_else(|| ConfigError::RepositoryNotConfigured(key.repository().to_owned()))?;
        repository
            .review_models
            .iter()
            .map(|model_id| {
                let model = self
                    .models
                    .iter()
                    .find(|model| model.id == *model_id)
                    .ok_or_else(|| ConfigError::UnknownReviewModel {
                        repository: repository.name.clone(),
                        model: model_id.clone(),
                    })?;
                for runner in &repository.runners {
                    let agent = self
                        .agents
                        .iter()
                        .find(|agent| agent.id == runner.agent)
                        .ok_or_else(|| ConfigError::UnknownAgent {
                            repository: repository.name.clone(),
                            agent: runner.agent.clone(),
                        })?;
                    if agent.supports(model_id) {
                        return Ok(ResolvedReviewModel {
                            repository: &repository.name,
                            model,
                            agent,
                            archive: &runner.archive,
                            execution: &self.execution,
                        });
                    }
                }
                Err(ConfigError::NoCompatibleAgent {
                    repository: repository.name.clone(),
                    model: model_id.clone(),
                })
            })
            .collect()
    }

    pub fn agents(&self) -> &[Agent] {
        &self.agents
    }
}

#[cfg(test)]
pub(crate) fn test_execution_json() -> serde_json::Value {
    serde_json::json!({
        "openshell": std::env::temp_dir().join("openshell"),
        "gateway": "test-gateway",
        "workspace": "default",
        "driver": "vm",
        "github_provider": "test-github-provider",
    })
}

#[cfg(test)]
pub(crate) fn test_runner_json() -> serde_json::Value {
    serde_json::json!({
        "kind": "claude",
        "model_provider": "test-model-provider",
        "policy": std::env::temp_dir().join("review-policy.yaml"),
        "policy_sha256": "a".repeat(64),
        "hold_command": ["/bin/sleep", "3600"]
    })
}

pub(crate) fn normalize_email(value: String) -> Result<String, ConfigError> {
    let valid = value.len() <= 254
        && value.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && !domain.is_empty() && !domain.contains('@') && domain.contains('.')
        })
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'<' | b'>' | b',' | b';'));
    if !valid {
        return Err(ConfigError::InvalidCoauthorEmail(value));
    }
    Ok(value.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vm_executable_paths_use_posix_absolute_syntax_on_every_host() {
        assert!(valid_vm_absolute_path("/usr/local/bin/claude"));
        for invalid in [
            "",
            "/",
            "bin/claude",
            "/usr/../claude",
            "/usr/./claude",
            "/usr//claude",
            "/usr/claude/",
            "C:\\claude.exe",
        ] {
            assert!(!valid_vm_absolute_path(invalid), "accepted {invalid:?}");
        }
    }

    fn archive_path(name: &str) -> String {
        std::env::temp_dir().join(name).to_str().unwrap().to_owned()
    }

    fn config_json() -> serde_json::Value {
        serde_json::json!({
            "schema_version": 5,
            "execution": test_execution_json(),
            "models": [
                {"id": "model-alpha", "coauthors": [{"name":"Model Alpha", "email":"model@example.com"}]},
                {"id": "model-beta", "coauthors": [{"name":"Model Beta", "email":"model@example.com"}]}
            ],
            "agents": [
                {
                    "id": "claude",
                    "models": ["model-alpha", "model-beta"],
                    "prompt": "Review the pull request.",
                    "runner": test_runner_json()
                },
                {
                    "id": "codex",
                    "models": ["model-beta"],
                    "prompt": "Find correctness and security issues.",
                    "runner": test_runner_json()
                }
            ],
            "repositories": [{
                "name": "Owner/Repo",
                "review_models": ["model-alpha", "model-beta"],
                "runners": [
                    {"agent": "codex", "vm_archive": {"path": archive_path("codex.tar"), "sha256": "b".repeat(64), "config_sha256": "c".repeat(64)}},
                    {"agent": "claude", "vm_archive": {"path": archive_path("claude.tar"), "sha256": "a".repeat(64), "config_sha256": "d".repeat(64)}}
                ]
            }]
        })
    }

    #[test]
    fn resolves_models_to_the_first_compatible_agent_runner() {
        let mut raw = config_json();
        raw["agents"][1]["runner"]["model_provider"] = "second-model-provider".into();
        let config = ReviewConfig::from_json(&serde_json::to_vec(&raw).unwrap()).unwrap();
        let review_models = config
            .review_models_for(&PrKey::new("owner/repo", 42).unwrap())
            .unwrap();
        assert_eq!(review_models.len(), 2);
        assert_eq!(review_models[0].model().id().as_str(), "model-alpha");
        assert_eq!(review_models[0].agent().id().as_str(), "claude");
        assert_eq!(review_models[1].model().id().as_str(), "model-beta");
        assert_eq!(review_models[1].agent().id().as_str(), "codex");
        assert_eq!(review_models[1].agent().runner().kind(), AgentKind::Claude);
        assert_eq!(
            review_models[1].agent().runner().model_provider(),
            "second-model-provider"
        );
        assert_eq!(
            review_models[0].agent().prompt(),
            "Review the pull request."
        );
        assert_eq!(review_models[0].agent().prompt_hash().as_str().len(), 64);
        assert!(review_models[0].archive().path().ends_with("claude.tar"));
        assert_eq!(
            review_models[0].archive().checksum().as_str(),
            "a".repeat(64)
        );
        assert!(review_models[0].model().matches_coauthor(&HashSet::from([
            CoauthorIdentity::new("Model Alpha", "MODEL@example.com").unwrap()
        ])));
        assert!(!review_models[1].model().matches_coauthor(&HashSet::from([
            CoauthorIdentity::new("Model Alpha", "model@example.com").unwrap()
        ])));
    }

    #[test]
    fn rejects_bad_archives_and_invalid_or_duplicate_configuration() {
        let mut raw = config_json();
        raw["repositories"][0]["runners"][0]["vm_archive"]["path"] = "relative.tar".into();
        assert!(matches!(
            ReviewConfig::from_json(&serde_json::to_vec(&raw).unwrap()),
            Err(ConfigError::InvalidArchivePath(_))
        ));

        let mut raw = config_json();
        raw["repositories"][0]["runners"][0]["vm_archive"]["sha256"] = "bad".into();
        assert!(matches!(
            ReviewConfig::from_json(&serde_json::to_vec(&raw).unwrap()),
            Err(ConfigError::InvalidArchiveChecksum)
        ));

        let mut raw = config_json();
        raw["agents"][0]["runner"]["kind"] = "unsupported".into();
        assert!(matches!(
            ReviewConfig::from_json(&serde_json::to_vec(&raw).unwrap()),
            Err(ConfigError::InvalidRunnerKind(_))
        ));

        let mut raw = config_json();
        raw["agents"][0]["runner"]["environment"] =
            serde_json::json!({"CLAUDE_CODE_EXECUTABLE":"/tmp/other"});
        assert!(matches!(
            ReviewConfig::from_json(&serde_json::to_vec(&raw).unwrap()),
            Err(ConfigError::InvalidRunner(_))
        ));

        let mut raw = config_json();
        raw["repositories"][0]["runners"][0]
            .as_object_mut()
            .unwrap()
            .remove("vm_archive");
        raw["repositories"][0]["runners"][0]["image"] = "quay.io/example/reviewer:latest".into();
        assert!(matches!(
            ReviewConfig::from_json(&serde_json::to_vec(&raw).unwrap()),
            Err(ConfigError::Json(_))
        ));

        let mut raw = config_json();
        raw["agents"][1]["id"] = "claude".into();
        assert!(matches!(
            ReviewConfig::from_json(&serde_json::to_vec(&raw).unwrap()),
            Err(ConfigError::DuplicateAgent(_))
        ));

        let mut raw = config_json();
        raw["repositories"][0]["runners"][1]["agent"] = "codex".into();
        assert!(matches!(
            ReviewConfig::from_json(&serde_json::to_vec(&raw).unwrap()),
            Err(ConfigError::DuplicateRunner { .. })
        ));

        let mut raw = config_json();
        raw["agents"][0]["prompt"] = "  ".into();
        assert!(matches!(
            ReviewConfig::from_json(&serde_json::to_vec(&raw).unwrap()),
            Err(ConfigError::InvalidPrompt(_))
        ));

        let mut raw = config_json();
        raw["agents"][0]["models"] = serde_json::json!(["model-beta"]);
        assert!(matches!(
            ReviewConfig::from_json(&serde_json::to_vec(&raw).unwrap()),
            Err(ConfigError::NoCompatibleAgent { .. })
        ));

        let mut raw = config_json();
        raw["models"][1]["coauthors"][0]["name"] = "Model Alpha".into();
        assert!(matches!(
            ReviewConfig::from_json(&serde_json::to_vec(&raw).unwrap()),
            Err(ConfigError::DuplicateCoauthorIdentity { .. })
        ));

        let mut raw = config_json();
        raw["schema_version"] = 1.into();
        assert!(matches!(
            ReviewConfig::from_json(&serde_json::to_vec(&raw).unwrap()),
            Err(ConfigError::UnsupportedSchema(1))
        ));

        let mut raw = config_json();
        raw["schema_version"] = 3.into();
        assert!(matches!(
            ReviewConfig::from_json(&serde_json::to_vec(&raw).unwrap()),
            Err(ConfigError::UnsupportedSchema(3))
        ));

        let mut raw = config_json();
        raw["schema_version"] = 4.into();
        assert!(matches!(
            ReviewConfig::from_json(&serde_json::to_vec(&raw).unwrap()),
            Err(ConfigError::UnsupportedSchema(4))
        ));

        let mut raw = config_json();
        raw["execution"]["mounts"] = serde_json::json!({
            "checkout": "/sandbox/other",
            "instructions": "/sandbox/instructions",
            "output": "/sandbox/output"
        });
        assert!(matches!(
            ReviewConfig::from_json(&serde_json::to_vec(&raw).unwrap()),
            Err(ConfigError::Json(_))
        ));
    }
}
