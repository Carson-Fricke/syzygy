//! Agent-selected model profiles shared by Rust and host-language adapters.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// An invalid agent catalog or profile selection.
#[derive(Debug)]
pub struct ConfigError(String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProfile {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Agent {
    model_profile: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    profiles: BTreeMap<String, ModelProfile>,
    agents: BTreeMap<String, Agent>,
}

pub struct AgentCatalog(Config);

impl AgentCatalog {
    /// Validate all agent links at load time, including currently unused roles.
    pub fn from_json(encoded: &str) -> Result<Self, ConfigError> {
        let config: Config = serde_json::from_str(encoded)
            .map_err(|e| ConfigError(format!("invalid agent catalog: {e}")))?;
        if config.profiles.is_empty() {
            return Err(ConfigError("agent catalog needs a model profile".into()));
        }
        for (id, profile) in &config.profiles {
            check_name("profile ID", id)?;
            check_name("provider", &profile.provider)?;
            check_name("model", &profile.model)?;
            if let Some(effort) = &profile.reasoning_effort {
                check_name("reasoning_effort", effort)?;
            }
        }
        for (role, agent) in &config.agents {
            check_name("agent role", role)?;
            check_name("model_profile", &agent.model_profile)?;
            if !config.profiles.contains_key(&agent.model_profile) {
                return Err(ConfigError(format!(
                    "agent {role:?} references unknown profile {:?}",
                    agent.model_profile
                )));
            }
        }
        Ok(Self(config))
    }

    pub fn profiles(&self) -> &BTreeMap<String, ModelProfile> {
        &self.0.profiles
    }

    /// An explicit profile must resolve precisely. Otherwise prefer the role's
    /// configured profile, falling back to the primary agent for unmapped roles.
    pub fn profile_id_for(&self, agent: &str, explicit: Option<&str>) -> Result<&str, ConfigError> {
        check_name("agent role", agent)?;
        let id = match explicit {
            Some(id) => id,
            None => {
                &self
                    .0
                    .agents
                    .get(agent)
                    .or_else(|| self.0.agents.get("primary"))
                    .ok_or_else(|| {
                        ConfigError(format!(
                            "agent {agent:?} has no selected profile and no primary fallback"
                        ))
                    })?
                    .model_profile
            }
        };
        self.0
            .profiles
            .get_key_value(id)
            .map(|(key, _)| key.as_str())
            .ok_or_else(|| ConfigError(format!("unknown model profile {id:?}")))
    }

    pub fn profile(
        &self,
        agent: &str,
        explicit: Option<&str>,
    ) -> Result<&ModelProfile, ConfigError> {
        Ok(&self.0.profiles[self.profile_id_for(agent, explicit)?])
    }
}

fn check_name(field: &str, value: &str) -> Result<(), ConfigError> {
    if value.trim().is_empty() {
        Err(ConfigError(format!(
            "agent configuration {field} must be nonempty"
        )))
    } else {
        Ok(())
    }
}
