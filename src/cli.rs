use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Human,
    Json,
}

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
    Run {
        task: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
        format: OutputFormat,
        #[arg(short = 'v', long)]
        verbose: bool,
        /// Allow the task to read files below this path. Repeatable; omitted means denied.
        #[arg(long = "allow-read", value_name = "PATH")]
        allow_read: Vec<PathBuf>,
        /// Allow the task to create or modify files below this path. Repeatable; omitted means denied.
        #[arg(long = "allow-write", value_name = "PATH")]
        allow_write: Vec<PathBuf>,
    },
}
