mod agent;
mod cli;
mod config;
mod model;
mod tools;

use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;

use crate::{agent::Agent, cli::Cli, config::Config, model::DeepSeek, tools::Tools};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut tools = Tools::new();
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
    let config_path = match cli.config {
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
    let cli::Mode::Run { task } = cli.command;
    agent.run(&task, tools, &mut std::io::stdout()).await
}

#[cfg(test)]
#[path = "../tests/support/agent.rs"]
mod integration_tests;
