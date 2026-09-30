// Copyright 2026 David Akermann
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

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
    #[serde(default)]
    pub api_key: String,
    pub base_url: Option<String>,
    pub request_timeout_secs: Option<u64>,
}

fn default_system_prompt() -> String {
    "You are executing a single non-interactive task. Use the available tools when needed. Complete the task and return the final result. Do not ask follow-up questions, request confirmation or permissions, suggest that the user proceed, or say that you will wait. If the task cannot be completed, report the concrete reason briefly and stop.".into()
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

    pub fn select_provider(&self, name: Option<&str>) -> Result<Option<&Provider>> {
        let provider = match name {
            Some(name) => Some(
                self.providers
                    .iter()
                    .find(|p| p.provider_name == name)
                    .with_context(|| format!("provider `{name}` is not configured"))?,
            ),
            None => self.enabled_provider(),
        };
        if let Some(provider) = provider {
            provider.validate_selected()?;
        }
        Ok(provider)
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
            if provider.provider_name == "deepseek" {
                ensure!(
                    provider.base_url.is_none() && provider.request_timeout_secs.is_none(),
                    "base_url and request_timeout_secs are only supported for llamacpp"
                );
            }
            if provider.enabled {
                provider.validate_selected()?;
            }
        }
        ensure!(
            self.providers.iter().filter(|p| p.enabled).count() <= 1,
            "multiple enabled providers; enable at most one provider"
        );
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

impl Provider {
    pub fn validate_selected(&self) -> Result<()> {
        ensure!(
            matches!(self.provider_name.as_str(), "deepseek" | "llamacpp"),
            "unsupported provider: {}",
            self.provider_name
        );
        ensure!(!self.model.trim().is_empty(), "model must not be empty");
        if self.provider_name == "deepseek" {
            ensure!(!self.api_key.trim().is_empty(), "api_key must not be empty");
            ensure!(
                self.base_url.is_none() && self.request_timeout_secs.is_none(),
                "base_url and request_timeout_secs are only supported for llamacpp"
            );
        } else {
            self.llamacpp_endpoint()?;
            ensure!(
                self.request_timeout_secs.unwrap_or(600) > 0,
                "request_timeout_secs must be greater than zero"
            );
        }
        Ok(())
    }

    pub fn llamacpp_endpoint(&self) -> Result<String> {
        let mut url = reqwest::Url::parse(
            self.base_url
                .as_deref()
                .unwrap_or("http://127.0.0.1:8080/v1"),
        )
        .map_err(|_| anyhow::anyhow!("invalid llamacpp base_url"))?;
        ensure!(
            matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
            "llamacpp base_url must be an HTTP or HTTPS URL"
        );
        ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "llamacpp base_url must not contain credentials, query strings, or fragments"
        );
        let path = format!("{}/chat/completions", url.path().trim_end_matches('/'));
        url.set_path(&path);
        Ok(url.to_string())
    }
}

#[cfg(test)]
mod tests;
