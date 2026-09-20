mod state;
mod stream;
mod tui;
mod view;

use std::{
    io::{self, BufRead, IsTerminal, Write},
    path::PathBuf,
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::{agent::Agent, tools::Tools};

#[derive(Parser)]
#[command(
    version,
    about = "A minimal agent harness with streaming, built-in and MCP tools"
)]
pub struct Cli {
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "Configuration file (default: $XDG_CONFIG_HOME/hadaka-agent/config.toml or ~/.config/hadaka-agent/config.toml)"
    )]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Mode,
}

#[derive(Subcommand)]
pub enum Mode {
    /// Start a conversation; /exit or EOF ends the session.
    Chat,
    /// Run a single task and exit.
    Run { task: String },
}

pub fn is_interactive(mode: &Mode) -> bool {
    matches!(mode, Mode::Chat) && io::stdin().is_terminal() && io::stdout().is_terminal()
}

#[derive(Debug)]
pub struct Interrupted;

impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("interrupted")
    }
}

impl std::error::Error for Interrupted {}

pub async fn run(
    mode: Mode,
    mut agent: Option<Agent>,
    tools: &Tools,
    setup_message: &str,
    diagnostics: tokio::sync::mpsc::UnboundedReceiver<String>,
) -> Result<()> {
    if is_interactive(&mode) {
        return tui::run(agent, tools, setup_message, diagnostics).await;
    }
    let mut output = io::stdout();
    match mode {
        Mode::Run { task } => {
            agent
                .as_mut()
                .with_context(|| setup_message.to_owned())?
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
                if line.trim() == "/settings" {
                    writeln!(output, "Not yet implemented")?;
                    continue;
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
