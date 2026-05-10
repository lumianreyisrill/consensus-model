use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Human-readable name
    pub name: String,
    /// API base URL (e.g., https://api.openai.com/v1)
    pub base_url: String,
    /// API key for authentication
    pub api_key: String,
    /// Model identifier (e.g., "kr/claude-sonnet-4.5")
    pub model: String,
    /// Optional: label for display
    pub label: Option<String>,
    /// Optional: max tokens override
    pub max_tokens: Option<u32>,
    /// Optional: timeout in seconds
    pub timeout_secs: Option<u64>,
}

impl ProviderConfig {
    pub fn display_name(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.name)
    }
}

/// Main configuration
#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    /// Provider definitions
    pub providers: Vec<ProviderConfig>,
    /// Default temperature
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    /// Default max tokens
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Request timeout in seconds
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Max retries on transient failures
    #[serde(default = "default_retries")]
    pub max_retries: u32,
    /// Path to SQLite history database
    #[serde(default = "default_history_db_path")]
    pub history_db_path: String,
}

fn default_temperature() -> f32 { 0.3 }
fn default_max_tokens() -> u32 { 4096 }
fn default_timeout() -> u64 { 120 }
fn default_retries() -> u32 { 2 }
fn default_history_db_path() -> String { "~/.config/consensus/history.db".to_string() }

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let expanded = expand_tilde(path);
        let content = std::fs::read_to_string(&expanded)
            .with_context(|| format!("Failed to read config: {}", expanded.display()))?;
        let config: Config = toml::from_str(&content)
            .with_context(|| format!("Failed to parse config: {}", expanded.display()))?;
        if config.providers.is_empty() {
            anyhow::bail!("No providers defined in config");
        }
        Ok(config)
    }

    pub fn provider_map(&self) -> HashMap<String, &ProviderConfig> {
        self.providers.iter().map(|p| (p.name.clone(), p)).collect()
    }

    /// Filter providers by name list (or return all if None)
    pub fn filter_providers(&self, names: Option<&[String]>) -> Vec<&ProviderConfig> {
        match names {
            Some(names) => self.providers.iter().filter(|p| names.contains(&p.name)).collect(),
            None => self.providers.iter().collect(),
        }
    }

    /// Get resolved history DB path
    pub fn history_db_path_resolved(&self) -> std::path::PathBuf {
        expand_tilde(Path::new(&self.history_db_path))
    }
}

/// Generate example config content
pub fn example_config() -> String {
    r#"# Consensus — Multi-Model AI Debate Engine
# Config file: ~/.config/consensus/config.toml

temperature = 0.3
max_tokens = 4096
timeout_secs = 120
max_retries = 2
history_db_path = "~/.config/consensus/history.db"

# Define your AI providers here.
# Each provider needs a name, base_url, api_key, and model.
# Works with any OpenAI-compatible API.

[[providers]]
name = "kiro"
base_url = "https://api.example.com/v1"
api_key = "your-api-key-here"
model = "claude-sonnet-4-5"
label = "🔵 Kiro (Claude)"

[[providers]]
name = "codex"
base_url = "https://api.example.com/v1"
api_key = "your-api-key-here"
model = "gpt-5"
label = "🟢 Codex (GPT)"

[[providers]]
name = "mimo"
base_url = "https://api.example.com/v1"
api_key = "your-api-key-here"
model = "mimo-v2.5-pro"
label = "🟣 MiMo (Xiaomi)"
"#.to_string()
}

fn expand_tilde(path: &Path) -> std::path::PathBuf {
    let s = path.to_string_lossy();
    if s.starts_with("~/") || s.starts_with("~\\") {
        if let Some(home) = dirs::home_dir() {
            return home.join(&s[2..]);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_tilde() {
        let path = Path::new("~/test/config.toml");
        let expanded = expand_tilde(path);
        assert!(!expanded.to_string_lossy().starts_with('~'));
    }

    #[test]
    fn test_example_config_parse() {
        let config: Config = toml::from_str(&example_config()).unwrap();
        assert_eq!(config.providers.len(), 3);
        assert_eq!(config.providers[0].name, "kiro");
    }

    #[test]
    fn test_history_db_path_default() {
        let config: Config = toml::from_str(&example_config()).unwrap();
        assert!(config.history_db_path.contains("history.db"));
    }
}
