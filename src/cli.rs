use std::path::PathBuf;

use clap::{Parser, Subcommand};

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
    /// Run a single task and exit.
    Run { task: String },
}
