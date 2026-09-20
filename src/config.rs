#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub providers: Vec<Provider>,
    #[serde(default = "default_system_prompt")]
    pub system_prompt: String,
    #[serde(default = "default_max_turns")]
    pub max_turns: usize,
    #[serde(default)]
    pub mcp_servers: Vec<McpServer>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub provider_name: String,
    pub enabled: bool,
    pub model: String,
    pub api_key: String,
}

fn default_system_prompt() -> String {
    "You are a helpful assistant. Use the available tools when needed. Answer the user when the task is complete or you need clarification.".into()
}

fn default_max_turns() -> usize {
    20
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: std::collections::HashMap<String, String>,
}

pub fn default_path() -> PathBuf {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    config_home.join("hadaka-agent").join("config.toml")
}

pub fn create_if_missing(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create config directory {}", parent.display()))?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    match options.open(path) {
        Ok(mut file) => file
            .write_all(include_bytes!("../agent.example.toml"))
            .with_context(|| format!("cannot write config {}", path.display())),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("cannot create config {}", path.display()))
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read config {}", path.display()))?;
        let table: toml::Table = toml::from_str(&source).context("invalid agent configuration")?;
        let config: Self = table.try_into().context("invalid agent configuration")?;
        config.validate()?;
        Ok(config)
    }

    pub fn enabled_provider(&self) -> Option<&Provider> {
        self.providers.iter().find(|provider| provider.enabled)
    }

    fn validate(&self) -> Result<()> {
        let mut provider_names = HashSet::new();
        for provider in &self.providers {
            ensure!(
                !provider.provider_name.trim().is_empty(),
                "provider_name must not be empty"
            );
            ensure!(
                provider_names.insert(&provider.provider_name),
                "duplicate provider_name: {}",
                provider.provider_name
            );
            if provider.enabled {
                ensure!(
                    provider.provider_name == "deepseek",
                    "unsupported provider: {}",
                    provider.provider_name
                );
                ensure!(!provider.model.trim().is_empty(), "model must not be empty");
                ensure!(
                    !provider.api_key.trim().is_empty(),
                    "api_key must not be empty"
                );
            }
        }
        ensure!(self.max_turns > 0, "max_turns must be greater than zero");
        let mut names = HashSet::new();
        for server in &self.mcp_servers {
            ensure!(
                !server.name.trim().is_empty(),
                "MCP server name must not be empty"
            );
            ensure!(
                names.insert(&server.name),
                "duplicate MCP server name: {}",
                server.name
            );
            ensure!(
                !server.command.trim().is_empty(),
                "MCP command must not be empty: {}",
                server.name
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn creates_config_file_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        create_if_missing(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn accepts_the_complete_example() {
        let config: Config = toml::from_str(include_str!("../agent.example.toml")).unwrap();
        config.validate().unwrap();
        assert!(config.mcp_servers.is_empty());
        assert!(config.enabled_provider().is_none());
    }

    #[test]
    fn rejects_duplicate_provider_names_even_when_disabled() {
        let mut source: toml::Table =
            toml::from_str(include_str!("../agent.example.toml")).unwrap();
        let providers = source.get_mut("providers").unwrap().as_array_mut().unwrap();
        providers.push(providers[0].clone());
        let config: Config = source.try_into().unwrap();
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("duplicate provider_name: deepseek")
        );
    }

    #[test]
    fn rejects_unknown_enabled_provider() {
        let mut config: Config = toml::from_str(include_str!("../agent.example.toml")).unwrap();
        config.providers[0].provider_name = "other".into();
        config.providers[0].enabled = true;
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("unsupported provider: other")
        );
    }

    #[test]
    fn requires_every_provider_field() {
        let source: toml::Table = toml::from_str(include_str!("../agent.example.toml")).unwrap();
        let source = source["providers"].as_array().unwrap()[0]
            .as_table()
            .unwrap();
        for field in ["provider_name", "enabled", "model", "api_key"] {
            let mut incomplete = source.clone();
            incomplete.remove(field);
            let error = toml::from_str::<Provider>(&incomplete.to_string())
                .err()
                .unwrap();
            assert!(
                error
                    .to_string()
                    .contains(&format!("missing field `{field}`")),
                "{error}"
            );
        }
    }

    #[test]
    fn requires_every_mcp_field_but_accepts_explicit_empty_values() {
        let source: toml::Table =
            toml::from_str("name = 'test'\ncommand = 'server'\nargs = []\nenv = {}").unwrap();
        let server: McpServer = toml::from_str(&source.to_string()).unwrap();
        assert!(server.args.is_empty());
        assert!(server.env.is_empty());
        for field in ["name", "command", "args", "env"] {
            let mut incomplete = source.clone();
            incomplete.remove(field);
            let error = toml::from_str::<McpServer>(&incomplete.to_string())
                .err()
                .unwrap();
            assert!(
                error
                    .to_string()
                    .contains(&format!("missing field `{field}`")),
                "{error}"
            );
        }
    }

    #[test]
    fn validates_explicit_values_and_rejects_unknown_fields() {
        let source: toml::Table = toml::from_str(include_str!("../agent.example.toml")).unwrap();
        for (field, value) in [
            ("max_turns", toml::Value::Integer(0)),
            ("model", toml::Value::String(String::new())),
            (
                "base_url",
                toml::Value::String("http://localhost/v1".into()),
            ),
            ("provider", toml::Value::String("other".into())),
        ] {
            let mut invalid = source.clone();
            invalid.insert(field.into(), value);
            let parsed = toml::from_str::<Config>(&invalid.to_string());
            assert!(
                parsed
                    .and_then(|c| c.validate().map_err(serde::de::Error::custom))
                    .is_err()
            );
        }
    }
}
