# 🧠 Consensus

**Multi-model AI debate engine.** Dispatch prompts to multiple LLM providers in parallel, cross-audit each response, and synthesize a consensus answer.

Eliminates single-model hallucination and blind spots through adversarial peer review.

```
consensus --mode code --prompt "Review this function" --file src/app.rs
```

## How It Works

```
                    ┌──────────────┐
                    │  Your Prompt  │
                    └──────┬───────┘
                           │
              ┌────────────┼────────────┐
              ▼            ▼            ▼
        ┌──────────┐ ┌──────────┐ ┌──────────┐
        │  Model A  │ │  Model B  │ │  Model C  │
        │ (parallel) │ │ (parallel) │ │ (parallel) │
        └────┬─────┘ └────┬─────┘ └────┬─────┘
             │             │             │
             └──────┬──────┘─────────────┘
                    ▼
           ┌─────────────────┐
           │  Cross-Audit    │  Each model reviews
           │  (Round 2)      │  the others' responses
           └────────┬────────┘
                    ▼
           ┌─────────────────┐
           │  Synthesis      │  Merge best parts
           │  (Round 3)      │  into final answer
           └────────┬────────┘
                    ▼
           ┌─────────────────┐
           │  History DB     │  Track model accuracy
           │  (auto-log)     │  Weighted voting over time
           └─────────────────┘
```

## 5 Debate Modes

| Mode | API Calls | When to Use |
|------|-----------|-------------|
| `quick` | 3 | Simple questions, fast answers |
| `general` | 6 | Research, analysis, opinions |
| `code` | 7 | Code review, architecture |
| `debug` | 7 | Error diagnosis, bug fixes |
| `adversarial` | 7 | Security audits, red-teaming |

## Install

```bash
# From source (requires Rust 1.75+)
git clone https://github.com/lumianreyisrill/consensus-model.git
cd consensus-model
cargo install --path .

# Binary will be at ~/.cargo/bin/consensus
```

## Quick Start

### 1. Create config

```bash
consensus --init
# Creates ~/.config/consensus/config.toml
```

### 2. Edit config with your providers

```toml
temperature = 0.3
max_tokens = 4096
timeout_secs = 120
max_retries = 2
history_db_path = "~/.config/consensus/history.db"  # optional

[[providers]]
name = "gpt"
base_url = "https://api.openai.com/v1"
api_key = "sk-your-key"
model = "gpt-4o"
label = "🔵 GPT-4o"

[[providers]]
name = "claude"
base_url = "https://api.anthropic.com/v1"
api_key = "sk-ant-your-key"
model = "claude-sonnet-4-20250514"
label = "🟢 Claude"

[[providers]]
name = "deepseek"
base_url = "https://api.openrouter.ai/v1"
api_key = "sk-or-your-key"
model = "deepseek/deepseek-chat"
label = "🟣 DeepSeek"
```

Works with **any OpenAI-compatible API**: OpenAI, Anthropic (via proxy), OpenRouter, Ollama, vLLM, local models, etc.

### 3. Run

```bash
# Quick question
consensus --mode quick --prompt "What is the time complexity of quicksort?"

# Code review
consensus --mode code --prompt "Review for bugs and security issues" --file src/app.rs

# Debug an error
consensus --mode debug --prompt "Why is this crashing?" --file error.log

# Security audit
consensus --mode adversarial --prompt "Find vulnerabilities" --file contract.sol

# Pipe input
cat logs/error.log | consensus --mode debug --prompt "Diagnose this error" --stdin

# JSON output for scripts
consensus --mode code --prompt "Review" --file app.py --output json | jq '.synthesis.content'

# Use specific providers only
consensus --mode quick --prompt "Hello" --providers gpt,claude

# Streaming output (real-time tokens as they arrive)
consensus --mode quick --prompt "Hello" --stream
```

## Scoreboard & History

Consensus automatically tracks model accuracy across debates using a local SQLite database.

```bash
# View model scoreboard — which model is most accurate
consensus scoreboard

# Output:
# Provider  Score  Debates  Best  Issues Found  Issues Received
# ──────────────────────────────────────────────────────────────
# gpt        72.3     15      8       42             12
# claude     68.1     15      5       38             18
# deepseek   45.0     15      2       25             30

# View debate history
consensus history
consensus history --limit 20
consensus history --provider gpt
```

**Score formula:** `(times_best × 3 + (debates - issues_received) × 2) / max(debates × 5, 1) × 100`

**Weighted voting:** After enough debates, the synthesis round automatically picks the highest-scoring model to produce the final answer.

**BEST detection:** The synthesis model ranks which initial response was most accurate. This feedback loops into scoring.

## CLI Reference

```
consensus [OPTIONS] --prompt <PROMPT>
consensus scoreboard
consensus history [--limit N] [--provider NAME]

Options:
  -m, --mode <MODE>          Debate mode [default: code]
                               [possible: quick, general, code, debug, adversarial]
  -p, --prompt <PROMPT>      Prompt/question for the models
  -f, --file <FILE>          File to include in the prompt (code, logs, etc)
      --stdin                Read additional input from stdin
  -c, --config <CONFIG>      Config file path [default: ~/.config/consensus/config.toml]
  -o, --output <OUTPUT>      Output format [default: text] [possible: text, json]
      --providers <PROVIDERS> Override which providers to use (comma-separated names)
      --temperature <TEMP>   Temperature (0.0-2.0) [default: 0.3]
      --max-tokens <N>       Max tokens per response [default: 4096]
  -q, --quiet                Suppress progress output
      --stream               Enable streaming output for Round 1 responses
      --init                 Create example config file
```

## Output Formats

### Text (default)

Rich terminal output with colors, progress indicators, and collapsible sections. Shows consensus answer prominently, individual responses and audit feedback in expandable blocks.

### JSON (`--output json`)

```json
{
  "mode": "CODE REVIEW",
  "responses": [
    {
      "provider": "gpt",
      "label": "🔵 GPT-4o",
      "model": "gpt-4o",
      "content": "...",
      "elapsed_secs": 14.3,
      "error": null,
      "tokens_used": { "prompt_tokens": 1500, "completion_tokens": 800, "total_tokens": 2300 }
    }
  ],
  "audits": [...],
  "synthesis": { "provider": "claude", "content": "Final merged answer..." },
  "total_secs": 142.3
}
```

## Architecture

```
src/
├── main.rs       # CLI entry, subcommand routing, config loading
├── cli.rs        # Clap argument parsing, subcommands, mode/output enums
├── config.rs     # TOML config parsing, provider management
├── debate.rs     # 3-round debate pipeline + scoring integration
├── api.rs        # OpenAI-compatible HTTP client, SSE parser, retry logic
├── history.rs    # SQLite history DB, model scoring, audit tracking
└── stream.rs     # Real-time SSE streaming output
```

**Key design decisions:**
- **Tokio JoinSet** for true parallel dispatch (not sequential)
- **SSE fallback** — handles both JSON and streaming responses
- **Retry with backoff** — exponential delay on transient failures (429, 5xx)
- **UTF-8 safe** truncation for error messages
- **No vendor lock-in** — works with any OpenAI-compatible endpoint
- **SQLite history** — local model scoring with weighted voting
- **Feature-gated** — `rusqlite` is optional (`--no-default-features` to build without history)

## Use Cases

- **Code review** — catch bugs one model misses
- **Security audit** — adversarial mode finds vulnerabilities
- **Research** — multiple perspectives on complex topics
- **Debugging** — cross-validate diagnosis before fixing
- **Writing** — synthesis merges the best prose from each model

## Requirements

- Rust 1.75+
- At least 2 OpenAI-compatible API providers configured

## License

MIT
