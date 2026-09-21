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

mod agent;
mod cli;
mod config;
mod model;
mod tools;

use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;

use crate::{
    agent::Agent,
    cli::Cli,
    config::Config,
    model::DeepSeek,
    tools::{PermissionPolicy, Tools},
};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let policy = PermissionPolicy::new(
        cli.allow_read.clone(),
        cli.allow_write.clone(),
        cli.allow_exec.clone(),
    )
    .with_network_hosts(cli.allow_net.clone());
    let mut tools = Tools::with_policy(policy);
    let mut interrupted = false;
    let result = tokio::select! {
        result = execute(cli, &mut tools) => result,
        signal = tokio::signal::ctrl_c() => {
            interrupted = signal.is_ok();
            signal.context("cannot listen for Ctrl-C")
        }
    };
    let cleanup = tools.shutdown().await;
    if let Err(error) = &result
        && !interrupted
    {
        eprintln!("error: {error:#}");
    }
    if let Err(error) = &cleanup {
        eprintln!("error: {error:#}");
    }
    if interrupted {
        eprintln!("interrupted");
        ExitCode::from(130)
    } else if result.is_err() || cleanup.is_err() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

async fn execute(cli: Cli, tools: &mut Tools) -> Result<()> {
    let cli::Cli {
        config,
        task,
        format,
        verbose,
        ..
    } = cli;
    let config_path = match config {
        Some(path) => path,
        None => {
            let path = config::default_path();
            config::create_if_missing(&path)?;
            path
        }
    };
    let config = Config::load(&config_path)?;
    let setup_message = format!(
        "A provider needs to be configured first. Set the DeepSeek provider's api_key and enabled = true in {}.",
        config_path.display()
    );
    let provider = config.enabled_provider().context(setup_message)?;
    let model = DeepSeek::new(provider)?;
    tools.connect(&config.mcp_servers).await?;
    let mut agent = Agent::new(model, config.system_prompt, config.max_turns);
    agent
        .run_with_options(&task, tools, &mut std::io::stdout(), format, verbose)
        .await
}

#[cfg(test)]
#[path = "../tests/support/agent.rs"]
mod integration_tests;
