mod agent;
mod config;
mod model;
mod tools;

use std::{
    io::{self, BufRead, Write},
    path::PathBuf,
    process::ExitCode,
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::{agent::Agent, config::Config, model::DeepSeek, tools::Tools};

#[derive(Parser)]
#[command(
    version,
    about = "A minimal DeepSeek agent with streaming, built-in and MCP tools"
)]
struct Cli {
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "Configuration file (default: $XDG_CONFIG_HOME/hadaka-agent/config.toml or ~/.config/hadaka-agent/config.toml)"
    )]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Mode,
}

#[derive(Subcommand)]
enum Mode {
    /// Start a conversation; /exit or EOF ends the session.
    Chat,
    /// Run a single task and exit.
    Run { task: String },
}

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
    if let Err(error) = &result {
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
    let mut agent = if let Some(provider) = config.enabled_provider() {
        let model = DeepSeek::new(provider)?;
        tools.connect(&config.mcp_servers).await?;
        Some(Agent::new(model, config.system_prompt, config.max_turns))
    } else {
        None
    };
    let mut output = io::stdout();
    match cli.command {
        Mode::Run { task } => {
            agent
                .as_mut()
                .context(setup_message)?
                .run(&task, tools, &mut output)
                .await
        }
        Mode::Chat => {
            if agent.is_none() {
                eprintln!("{setup_message}");
            }
            eprintln!("Chat ready. Type /exit or press Ctrl-D to exit.");
            let mut input = input_lines();
            loop {
                eprint!("you> ");
                io::stderr().flush()?;
                let Some(line) = input.recv().await else {
                    return Ok(());
                };
                let line = line.context("cannot read terminal input")?;
                if line.trim() == "/exit" {
                    return Ok(());
                }
                if line.trim().is_empty() {
                    continue;
                }
                if let Some(agent) = &mut agent {
                    agent.run(&line, tools, &mut output).await?;
                } else {
                    eprintln!("{setup_message}");
                }
            }
        }
    }
}

fn input_lines() -> tokio::sync::mpsc::Receiver<io::Result<String>> {
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    // Tokio stdin uses a blocking runtime task that cannot be cancelled. A plain
    // reader thread lets Ctrl-C exit immediately even while waiting for a line.
    std::thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            if sender.blocking_send(line).is_err() {
                break;
            }
        }
    });
    receiver
}

#[cfg(test)]
#[path = "../tests/support/agent.rs"]
mod integration_tests;
