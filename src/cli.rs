use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "consensus",
    about = "Multi-model AI debate engine — dispatch, cross-audit, synthesize consensus",
    version,
    long_about = "Dispatches prompts to multiple LLM providers in parallel, cross-audits \nresponses from each model, and synthesizes a consensus answer. \nEliminates single-model hallucination and blind spots."
)]
pub struct Cli {
    /// Debate mode
    #[arg(short, long, default_value = "code")]
    pub mode: DebateMode,

    /// Prompt/question for the models (required unless --init)
    #[arg(short, long)]
    pub prompt: Option<String>,

    /// File to include in the prompt (code, logs, etc)
    #[arg(short, long)]
    pub file: Option<PathBuf>,

    /// Read additional input from stdin
    #[arg(long)]
    pub stdin: bool,

    /// Config file path
    #[arg(short, long, default_value = "~/.config/consensus/config.toml")]
    pub config: PathBuf,

    /// Output format
    #[arg(short, long, default_value = "text")]
    pub output: OutputFormat,

    /// Override which providers to use (comma-separated names from config)
    #[arg(long, value_delimiter = ',')]
    pub providers: Option<Vec<String>>,

    /// Temperature (0.0-2.0)
    #[arg(long, default_value = "0.3")]
    pub temperature: f32,

    /// Max tokens per response
    #[arg(long, default_value = "4096")]
    pub max_tokens: u32,

    /// Suppress progress output
    #[arg(short, long)]
    pub quiet: bool,

    /// Create example config file at --config path
    #[arg(long)]
    pub init: bool,

    /// Enable streaming output for Round 1 responses
    #[arg(long)]
    pub stream: bool,

    /// Subcommand (scoreboard, history)
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Show model scorecard from debate history
    Scoreboard,
    /// Show recent debate history
    History {
        /// Number of debates to show
        #[arg(long, default_value = "10")]
        limit: usize,
        /// Filter by provider name
        #[arg(long)]
        provider: Option<String>,
    },
}

#[derive(Clone, ValueEnum)]
pub enum DebateMode {
    /// 3 parallel responses, no audit (3 API calls)
    Quick,
    /// 3 responses + cross-audit (6 calls)
    General,
    /// 3 + audit + synthesis (7 calls)
    Code,
    /// 3 + audit + synthesis (7 calls)
    Debug,
    /// 3 + adversarial audit + synthesis (7 calls)
    Adversarial,
}

#[derive(Clone, ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
}
