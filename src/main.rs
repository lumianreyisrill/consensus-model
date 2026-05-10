mod api;
mod cli;
mod config;
mod debate;
mod history;
mod stream;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands};
use config::Config;
use std::io::Read;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Handle --init
    if cli.init {
        let config_path = expand_path(&cli.config);
        if config_path.exists() {
            eprintln!("Config already exists at {}", config_path.display());
            std::process::exit(0);
        }
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&config_path, config::example_config())?;
        eprintln!("✅ Config created at {}", config_path.display());
        eprintln!("Edit it with your API keys, then run consensus again.");
        std::process::exit(0);
    }

    // Handle subcommands that don't need a full debate
    if let Some(ref cmd) = cli.command {
        return handle_subcommand(cmd, &cli).await;
    }

    // Load config
    let config_path = expand_path(&cli.config);
    let config = Config::load(&config_path).map_err(|e| {
        if e.to_string().contains("No such file") {
            anyhow::anyhow!(
                "Config not found at {}. Create one or use --config <path>",
                config_path.display()
            )
        } else {
            e
        }
    })?;

    // Validate prompt
    let prompt = cli.prompt.unwrap_or_else(|| {
        eprintln!("Error: --prompt is required (unless using --init)");
        std::process::exit(1);
    });

    // Validate providers
    let providers = config.filter_providers(cli.providers.as_deref());
    if providers.len() < 2 {
        anyhow::bail!(
            "Need at least 2 providers. Found {}. Edit {} to add more.",
            providers.len(),
            config_path.display()
        );
    }

    // Build user message
    let mut user_msg = prompt;

    // Read file input
    if let Some(ref file_path) = cli.file {
        let expanded = expand_path(file_path);
        let content = std::fs::read_to_string(&expanded)
            .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", expanded.display(), e))?;
        user_msg = format!("{}\n\n```\n{}\n```", user_msg, content);
    }

    // Read stdin if requested
    if cli.stdin {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        if !buf.trim().is_empty() {
            user_msg = format!("{}\n\n{}", user_msg, buf);
        }
    }

    // Run debate
    let result = debate::run_debate(
        &config,
        &cli.mode,
        &user_msg,
        cli.providers.as_deref(),
        cli.temperature,
        cli.max_tokens,
        cli.quiet,
        cli.stream,
    )
    .await?;

    // Output
    match cli.output {
        cli::OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        cli::OutputFormat::Text => {
            println!("{}", debate::format_text(&result));
        }
    }

    // Exit with error if all providers failed
    if result.responses.iter().all(|r| r.error.is_some()) {
        std::process::exit(1);
    }

    Ok(())
}

async fn handle_subcommand(cmd: &Commands, cli: &Cli) -> Result<()> {
    let config_path = expand_path(&cli.config);

    match cmd {
        Commands::Scoreboard => {
            let config = Config::load(&config_path).map_err(|e| {
                if e.to_string().contains("No such file") {
                    anyhow::anyhow!(
                        "Config not found at {}. Create one or use --config <path>",
                        config_path.display()
                    )
                } else {
                    e
                }
            })?;

            #[cfg(feature = "history")]
            {
                let db_path = config.history_db_path_resolved();
                let db = history::history::HistoryDb::open(&db_path)?;
                let scores = db.get_scores()?;
                println!("{}", debate::format_scoreboard(&scores));
            }
            #[cfg(not(feature = "history"))]
            {
                let _ = config;
                eprintln!("History feature not compiled. Rebuild with `--features history`");
                std::process::exit(1);
            }
            Ok(())
        }
        Commands::History { limit, provider } => {
            let config = Config::load(&config_path).map_err(|e| {
                if e.to_string().contains("No such file") {
                    anyhow::anyhow!(
                        "Config not found at {}. Create one or use --config <path>",
                        config_path.display()
                    )
                } else {
                    e
                }
            })?;

            #[cfg(feature = "history")]
            {
                let db_path = config.history_db_path_resolved();
                let db = history::history::HistoryDb::open(&db_path)?;
                let summaries = db.get_history(*limit, provider.as_deref())?;
                println!("{}", debate::format_history(&summaries));
            }
            #[cfg(not(feature = "history"))]
            {
                let _ = (config, limit, provider);
                eprintln!("History feature not compiled. Rebuild with `--features history`");
                std::process::exit(1);
            }
            Ok(())
        }
    }
}

fn expand_path(path: &std::path::Path) -> std::path::PathBuf {
    let s = path.to_string_lossy();
    if s.starts_with("~/") || s.starts_with("~\\") {
        if let Some(home) = dirs::home_dir() {
            return home.join(&s[2..]);
        }
    }
    path.to_path_buf()
}
