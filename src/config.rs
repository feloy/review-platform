//! Validated model choices, compatible agent runners, and VM archives.

use crate::{PrKey, StateError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
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
        "invalid model ID {0:?}; use 1-128 lowercase ASCII letters, digits, dots, slashes, colons, hyphens, or underscores"
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
    #[error("VM archive SHA-256 must contain 64 hexadecimal characters")]
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

/// A local rootfs tar archive and the checksum expected by the lead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawVmArchive")]
pub struct VmArchive {
    path: PathBuf,
    sha256: ArchiveHash,
}

impl VmArchive {
    pub fn new(path: PathBuf, sha256: ArchiveHash) -> Result<Self, ConfigError> {
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
        Ok(Self { path, sha256 })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn checksum(&self) -> &ArchiveHash {
        &self.sha256
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawVmArchive {
    path: PathBuf,
    sha256: String,
}

impl TryFrom<RawVmArchive> for VmArchive {
    type Error = ConfigError;

    fn try_from(value: RawVmArchive) -> Result<Self, Self::Error> {
        Self::new(value.path, ArchiveHash::new(value.sha256)?)
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

    fn of(prompt: &str) -> Self {
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
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    schema_version: u32,
    models: Vec<RawModel>,
    agents: Vec<RawAgent>,
    repositories: Vec<RawRepository>,
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
    pub fn from_json(bytes: &[u8]) -> Result<Self, ConfigError> {
        let header: SchemaHeader = serde_json::from_slice(bytes)?;
        if header.schema_version != 2 {
            return Err(ConfigError::UnsupportedSchema(header.schema_version));
        }
        let raw: RawConfig = serde_json::from_slice(bytes)?;
        if raw.schema_version != 2 {
            return Err(ConfigError::UnsupportedSchema(raw.schema_version));
        }
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
        };
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

    fn archive_path(name: &str) -> String {
        std::env::temp_dir().join(name).to_str().unwrap().to_owned()
    }

    fn config_json() -> serde_json::Value {
        serde_json::json!({
            "schema_version": 2,
            "models": [
                {"id": "model-alpha", "coauthors": [{"name":"Model Alpha", "email":"model@example.com"}]},
                {"id": "model-beta", "coauthors": [{"name":"Model Beta", "email":"model@example.com"}]}
            ],
            "agents": [
                {
                    "id": "claude",
                    "models": ["model-alpha", "model-beta"],
                    "prompt": "Review the pull request."
                },
                {
                    "id": "codex",
                    "models": ["model-beta"],
                    "prompt": "Find correctness and security issues."
                }
            ],
            "repositories": [{
                "name": "Owner/Repo",
                "review_models": ["model-alpha", "model-beta"],
                "runners": [
                    {"agent": "codex", "vm_archive": {"path": archive_path("codex.tar"), "sha256": "b".repeat(64)}},
                    {"agent": "claude", "vm_archive": {"path": archive_path("claude.tar"), "sha256": "a".repeat(64)}}
                ]
            }]
        })
    }

    #[test]
    fn resolves_models_to_the_first_compatible_agent_runner() {
        let config = ReviewConfig::from_json(&serde_json::to_vec(&config_json()).unwrap()).unwrap();
        let review_models = config
            .review_models_for(&PrKey::new("owner/repo", 42).unwrap())
            .unwrap();
        assert_eq!(review_models.len(), 2);
        assert_eq!(review_models[0].model().id().as_str(), "model-alpha");
        assert_eq!(review_models[0].agent().id().as_str(), "claude");
        assert_eq!(review_models[1].model().id().as_str(), "model-beta");
        assert_eq!(review_models[1].agent().id().as_str(), "codex");
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
    }
}
