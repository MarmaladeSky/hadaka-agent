use std::path::PathBuf;

use clap::{Parser, ValueEnum};

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Human,
    Json,
}

#[derive(Parser)]
#[command(
    version,
    about = "A minimal agent harness with streaming, built-in and MCP tools",
    after_help = "AVAILABLE TOOLS:\n  echo             Return supplied text unchanged.\n  list_directory   List files and directories (requires --allow-read).\n  read_file        Read a UTF-8 text file (requires --allow-read).\n  text_editor      Create or edit text files (requires --allow-write).\n  MCP tools        Discovered from the configured MCP servers.\n\nPERMISSIONS:\n  --allow-read PATH    Allow reading files below PATH; repeatable.\n  --allow-write PATH   Allow creating or modifying files below PATH; repeatable.\n\nFile permissions are denied by default. Read and write permissions are independent.\nMCP server permissions are not restricted by these flags yet."
)]
pub struct Cli {
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "Configuration file (default: $XDG_CONFIG_HOME/hadaka-agent/config.toml or ~/.config/hadaka-agent/config.toml)"
    )]
    pub config: Option<PathBuf>,
    /// Task to execute.
    #[arg(value_name = "TASK")]
    pub task: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    pub format: OutputFormat,
    #[arg(short = 'v', long)]
    pub verbose: bool,
    /// Allow the task to read files below this path. Repeatable; omitted means denied.
    #[arg(long = "allow-read", value_name = "PATH")]
    pub allow_read: Vec<PathBuf>,
    /// Allow the task to create or modify files below this path. Repeatable; omitted means denied.
    #[arg(long = "allow-write", value_name = "PATH")]
    pub allow_write: Vec<PathBuf>,
}
