use crate::api::{ApiClient, ChatMessage, ModelResponse};
pub use crate::cli::DebateMode;
use crate::config::Config;
use anyhow::Result;
use colored::*;
use serde::{Deserialize, Serialize};

/// Result from a full debate cycle
#[derive(Debug, Serialize, Deserialize)]
pub struct DebateResult {
    pub mode: String,
    pub responses: Vec<ModelResponse>,
    pub audits: Vec<ModelResponse>,
    pub synthesis: Option<ModelResponse>,
    pub total_secs: f64,
}

/// System prompts for each mode
fn system_prompt(mode: &DebateMode) -> &'static str {
    match mode {
        DebateMode::Quick => {
            "Answer directly and concisely. No fluff. If uncertain, say so clearly."
        }
        DebateMode::General => "You are a senior analyst. Provide a thorough, well-reasoned answer. Be specific, cite evidence, and note uncertainties. Structure your answer with clear sections.",
        DebateMode::Code => "You are a senior software engineer performing code review. Analyze for: bugs, security vulnerabilities, performance issues, edge cases, readability, and best practices. Be specific — cite line numbers, suggest fixes. Rate severity: CRITICAL / HIGH / MEDIUM / LOW for each issue.",
        DebateMode::Debug => "You are a senior debugger. Given an error and code context, diagnose the root cause. Explain WHY the error happens, not just WHAT. Provide a specific fix with code. Verify your fix would actually resolve the error.",
        DebateMode::Adversarial => "You are a red-team security auditor. Your goal is to BREAK the code/design. Find every vulnerability, edge case, exploit vector, and failure mode. Be aggressive and creative. Think like an attacker. For each issue: describe the attack, its impact, and a fix.",
    }
}

const AUDIT_SYSTEM: &str = "You are a code auditor reviewing another AI's response. \
Your job is to find FLAWS, INACCURACIES, and MISSING PIECES. \
Be critical — don't agree just to be polite. \
For each issue found:\n\
1. Quote the problematic part\n\
2. Explain why it's wrong/incomplete\n\
3. Provide the correct answer or fix\n\
If the response is correct, say so explicitly and note any improvements.";

const SYNTHESIS_SYSTEM: &str =
    "You are a senior technical lead synthesizing multiple expert opinions. \
You receive: the original task, responses from multiple AI models, \
and cross-audit feedback from each model reviewing the others.\n\n\
Your job:\n\
1. Identify where ALL models agree → high confidence\n\
2. Identify disagreements → pick the best answer with reasoning\n\
3. Merge the best parts from each response\n\
4. Flag any remaining uncertainties\n\
5. Produce a single, definitive, final answer\n\n\
Format: Start with the answer/recommendation, then supporting analysis, \
then a summary of what was merged from whom.\n\n\
At the end of your response, on a new line, output 'BEST: <provider_name>' \
where provider_name is the name of the model that gave the most accurate and helpful initial response.";

/// Parse BEST: line from synthesis output. Returns (best_provider_name, cleaned_content).
pub fn parse_best_from_synthesis(content: &str) -> (Option<String>, String) {
    let lines: Vec<&str> = content.lines().collect();
    // Check last few lines for BEST: pattern
    for i in (0..lines.len()).rev().take(5) {
        let trimmed = lines[i].trim();
        if let Some(rest) = trimmed.strip_prefix("BEST:") {
            let name = rest.trim();
            if !name.is_empty() {
                // Remove the BEST: line and any trailing empty lines
                let mut cleaned_lines: Vec<&str> = lines[..i].to_vec();
                // Remove trailing empty lines
                while cleaned_lines.last().map_or(false, |l| l.trim().is_empty()) {
                    cleaned_lines.pop();
                }
                return (Some(name.to_string()), cleaned_lines.join("\n"));
            }
        }
    }
    (None, content.to_string())
}

/// Run the full debate pipeline
pub async fn run_debate(
    config: &Config,
    mode: &DebateMode,
    prompt: &str,
    provider_names: Option<&[String]>,
    temperature: f32,
    max_tokens: u32,
    quiet: bool,
    stream: bool,
) -> Result<DebateResult> {
    let providers = config.filter_providers(provider_names);

    if providers.len() < 2 {
        anyhow::bail!(
            "Need at least 2 providers for debate (found {})",
            providers.len()
        );
    }

    let user_msg = prompt.to_string();

    let start = std::time::Instant::now();

    // ── Round 1: Parallel initial responses ──────────────────────────────
    if !quiet {
        eprintln!("\n{}", "═".repeat(60).bright_blue());
        eprintln!(
            "  🎯 {} — {} providers",
            format!("DEBATE: {}", mode_name(mode))
                .bright_white()
                .bold(),
            providers.len()
        );
        eprintln!("{}\n", "═".repeat(60).bright_blue());
        eprintln!(
            "📡 {}: Dispatching to {} providers in parallel{}...",
            "Round 1".bright_cyan(),
            providers.len(),
            if stream { " (streaming)" } else { "" }
        );
    }

    let initial_messages = vec![
        ChatMessage {
            role: "system".into(),
            content: system_prompt(mode).into(),
        },
        ChatMessage {
            role: "user".into(),
            content: user_msg.clone(),
        },
    ];

    // Run all in parallel using tokio JoinSet
    let mut responses = Vec::new();
    {
        let mut set = tokio::task::JoinSet::new();
        for provider in &providers {
            let prov = (*provider).clone();
            let msgs = initial_messages.clone();
            let temp = temperature;
            let mt = prov.max_tokens.unwrap_or(max_tokens);
            let timeout = config.timeout_secs;
            let retries = config.max_retries;
            let use_stream = stream && matches!(mode, DebateMode::Code | DebateMode::Debug | DebateMode::Adversarial | DebateMode::General | DebateMode::Quick);

            set.spawn(async move {
                if use_stream && !prov.api_key.is_empty() {
                    // Try streaming first, fall back to non-streaming on error
                    match crate::stream::call_model_stream(&prov, msgs.clone(), temp, mt, timeout).await {
                        Ok(sr) if sr.error.is_none() => {
                            ModelResponse {
                                provider: prov.name.clone(),
                                label: prov.display_name().to_string(),
                                model: prov.model.clone(),
                                content: sr.content,
                                elapsed_secs: sr.elapsed_secs,
                                error: None,
                                tokens_used: sr.tokens_used,
                            }
                        }
                        _ => {
                            // Fall back to non-streaming
                            let task_client = ApiClient::new(timeout, retries).expect("client");
                            task_client.call_model(&prov, msgs, temp, mt).await
                        }
                    }
                } else {
                    let task_client = ApiClient::new(timeout, retries).expect("client");
                    task_client.call_model(&prov, msgs, temp, mt).await
                }
            });
        }

        while let Some(result) = set.join_next().await {
            match result {
                Ok(resp) => {
                    if !quiet {
                        if let Some(ref err) = resp.error {
                            eprintln!("  {}: ❌ {}", resp.label.red(), err);
                        } else {
                            eprintln!(
                                "  {}: ✅ {} chars ({:.1}s)",
                                resp.label.green(),
                                resp.content.len(),
                                resp.elapsed_secs
                            );
                        }
                    }
                    responses.push(resp);
                }
                Err(e) => {
                    if !quiet {
                        eprintln!("  Task error: {}", e);
                    }
                }
            }
        }
    }

    // Quick mode: return responses only
    if matches!(mode, DebateMode::Quick) {
        return Ok(DebateResult {
            mode: mode_name(mode).into(),
            responses,
            audits: vec![],
            synthesis: None,
            total_secs: start.elapsed().as_secs_f64(),
        });
    }

    // ── Round 2: Cross-audit ─────────────────────────────────────────────
    if !quiet {
        eprintln!(
            "\n🔍 {}: Cross-audit (each model reviews the others)...",
            "Round 2".bright_cyan()
        );
    }

    let valid_responses: Vec<&ModelResponse> =
        responses.iter().filter(|r| r.error.is_none()).collect();
    let mut audits = Vec::new();

    if valid_responses.len() >= 2 {
        let mut audit_set = tokio::task::JoinSet::new();

        for auditor in &providers {
            let others: Vec<String> = valid_responses
                .iter()
                .filter(|r| r.provider != auditor.name)
                .map(|r| format!("--- {} RESPONSE ---\n{}", r.label, r.content))
                .collect();

            if others.is_empty() {
                continue;
            }

            let other_text = others.join("\n\n");
            let audit_prompt = format!(
                "ORIGINAL TASK:\n{}\n\nRESPONSES TO AUDIT:\n{}\n\nReview each response. Find errors, gaps, and improvements.",
                prompt, other_text
            );

            let audit_messages = vec![
                ChatMessage {
                    role: "system".into(),
                    content: AUDIT_SYSTEM.into(),
                },
                ChatMessage {
                    role: "user".into(),
                    content: audit_prompt,
                },
            ];

            let prov = (*auditor).clone();
            let timeout = config.timeout_secs;
            let retries = config.max_retries;
            audit_set.spawn(async move {
                let task_client = ApiClient::new(timeout, retries).expect("client");
                task_client
                    .call_model(&prov, audit_messages, 0.2, 3000)
                    .await
            });
        }

        while let Some(result) = audit_set.join_next().await {
            match result {
                Ok(resp) => {
                    if !quiet {
                        if let Some(ref err) = resp.error {
                            eprintln!("  {}: ❌ {}", resp.label.red(), err);
                        } else {
                            eprintln!(
                                "  {}: ✅ {} chars ({:.1}s)",
                                resp.label.green(),
                                resp.content.len(),
                                resp.elapsed_secs
                            );
                        }
                    }
                    audits.push(resp);
                }
                Err(e) => {
                    if !quiet {
                        eprintln!("  Audit task error: {}", e);
                    }
                }
            }
        }
    }

    // General mode: return with audits, no synthesis
    if matches!(mode, DebateMode::General) {
        return Ok(DebateResult {
            mode: mode_name(mode).into(),
            responses,
            audits,
            synthesis: None,
            total_secs: start.elapsed().as_secs_f64(),
        });
    }

    // ── Round 3: Synthesis ───────────────────────────────────────────────
    if !quiet {
        eprintln!(
            "\n🧠 {}: Synthesis (merging all feedback)...",
            "Round 3".bright_cyan()
        );
    }

    let mut synthesis_parts = vec![format!("ORIGINAL TASK:\n{}\n", prompt)];

    synthesis_parts.push("\n--- MODEL RESPONSES ---".into());
    for r in &responses {
        if r.error.is_none() {
            synthesis_parts.push(format!("\n{}:\n{}", r.label, r.content));
        }
    }

    synthesis_parts.push("\n\n--- CROSS-AUDIT FEEDBACK ---".into());
    for a in &audits {
        if a.error.is_none() {
            synthesis_parts.push(format!("\n{} reviewed others:\n{}", a.label, a.content));
        }
    }

    let synthesis_messages = vec![
        ChatMessage {
            role: "system".into(),
            content: SYNTHESIS_SYSTEM.into(),
        },
        ChatMessage {
            role: "user".into(),
            content: synthesis_parts.join("\n"),
        },
    ];

    // Weighted voting: pick provider with highest historical score
    let synthesis_provider = if let Ok(Some(best_name)) = get_best_provider_safe(config).await {
        if !quiet {
            eprintln!(
                "  🏆 Synthesis provider: {} (highest historical score)",
                best_name.bright_yellow()
            );
        }
        providers.iter().find(|p| p.name == best_name).unwrap_or(providers.first().unwrap())
    } else {
        providers.first().unwrap()
    };

    if !quiet && !stream {
        eprintln!(
            "  Using {} for synthesis",
            synthesis_provider.display_name().bright_white()
        );
    }

    let synthesis_client =
        ApiClient::new(config.timeout_secs, config.max_retries)?;
    let synthesis = synthesis_client
        .call_model(synthesis_provider, synthesis_messages, 0.2, 4096)
        .await;

    // Parse BEST: line from synthesis content
    let (best_provider, cleaned_content) = if synthesis.error.is_none() {
        let (bp, cleaned) = parse_best_from_synthesis(&synthesis.content);
        if let Some(ref name) = bp {
            if !quiet {
                eprintln!("  🏆 Best initial response: {}", name.bright_green());
            }
        }
        (bp, cleaned)
    } else {
        (None, synthesis.content.clone())
    };

    // Create cleaned synthesis response
    let cleaned_synthesis = ModelResponse {
        content: cleaned_content,
        ..synthesis
    };

    if !quiet {
        if let Some(ref err) = cleaned_synthesis.error {
            eprintln!("  Synthesis: ❌ {}", err);
        } else {
            eprintln!(
                "  Synthesis: ✅ {} chars ({:.1}s)",
                cleaned_synthesis.content.len(),
                cleaned_synthesis.elapsed_secs
            );
        }
        eprintln!("\n{}", "═".repeat(60).bright_blue());
        eprintln!("  ⏱️  Total: {:.1}s", start.elapsed().as_secs_f64());
        eprintln!("{}\n", "═".repeat(60).bright_blue());
    }

    // Log to history if available
    let result = DebateResult {
        mode: mode_name(mode).into(),
        responses,
        audits,
        synthesis: Some(cleaned_synthesis),
        total_secs: start.elapsed().as_secs_f64(),
    };

    log_to_history(config, &result, prompt, best_provider.as_deref(), quiet);

    Ok(result)
}

/// Try to get best provider from history DB, silently return None on any error
async fn get_best_provider_safe(config: &Config) -> Result<Option<String>> {
    #[cfg(feature = "history")]
    {
        let db_path = config.history_db_path_resolved();
        match crate::history::history::HistoryDb::open(&db_path) {
            Ok(db) => db.get_best_provider(),
            Err(_) => Ok(None),
        }
    }
    #[cfg(not(feature = "history"))]
    {
        let _ = config;
        Ok(None)
    }
}

/// Log debate results to history database
fn log_to_history(config: &Config, result: &DebateResult, prompt: &str, best: Option<&str>, quiet: bool) {
    #[cfg(feature = "history")]
    {
        let db_path = config.history_db_path_resolved();
        match crate::history::history::HistoryDb::open(&db_path) {
            Ok(db) => {
                match db.log_debate(result, prompt, best) {
                    Ok(_) => {
                        if !quiet {
                            eprintln!("  💾 Logged to history DB");
                        }
                    }
                    Err(e) => {
                        if !quiet {
                            eprintln!("  ⚠️  Failed to log to history: {}", e);
                        }
                    }
                }
            }
            Err(e) => {
                if !quiet {
                    eprintln!("  ⚠️  Failed to open history DB: {}", e);
                }
            }
        }
    }
    #[cfg(not(feature = "history"))]
    {
        let _ = (config, result, prompt, best, quiet);
    }
}

fn mode_name(mode: &DebateMode) -> &'static str {
    match mode {
        DebateMode::Quick => "QUICK",
        DebateMode::General => "GENERAL",
        DebateMode::Code => "CODE REVIEW",
        DebateMode::Debug => "DEBUG",
        DebateMode::Adversarial => "ADVERSARIAL",
    }
}

/// Format debate result as rich terminal output
pub fn format_text(result: &DebateResult) -> String {
    let mut out = String::new();

    // Stats
    let successful = result.responses.iter().filter(|r| r.error.is_none()).count();
    out.push_str(&format!(
        "**Models consulted:** {}/{}\n",
        successful,
        result.responses.len()
    ));
    if let Some(ref synth) = result.synthesis {
        out.push_str(&format!("**Synthesized by:** {}\n", synth.label));
    }
    out.push('\n');

    // Main result (synthesis or best response)
    if let Some(ref synth) = result.synthesis {
        out.push_str("## ✅ Consensus Answer\n\n");
        out.push_str(&synth.content);
    } else {
        // No synthesis — show all responses
        for r in &result.responses {
            out.push_str(&format!(
                "## {}\n*({:.1}s)*\n\n",
                r.label, r.elapsed_secs
            ));
            if let Some(ref err) = r.error {
                out.push_str(&format!("❌ Error: {}\n\n", err));
            } else {
                out.push_str(&r.content);
                out.push_str("\n\n");
            }
        }
    }

    // Individual responses (collapsed)
    if result.synthesis.is_some() {
        out.push_str(
            "\n\n<details><summary>📋 Individual Responses (click to expand)</summary>\n\n",
        );
        for r in &result.responses {
            out.push_str(&format!(
                "### {} *({:.1}s)*\n",
                r.label, r.elapsed_secs
            ));
            if let Some(ref err) = r.error {
                out.push_str(&format!("❌ {}\n\n", err));
            } else {
                out.push_str(&r.content);
                out.push_str("\n\n");
            }
        }
        out.push_str("</details>\n");
    }

    // Audit feedback (collapsed)
    if !result.audits.is_empty() {
        out.push_str(
            "\n<details><summary>🔍 Cross-Audit Feedback (click to expand)</summary>\n\n",
        );
        for a in &result.audits {
            out.push_str(&format!("### {} → reviewed others\n", a.label));
            if let Some(ref err) = a.error {
                out.push_str(&format!("❌ {}\n\n", err));
            } else {
                out.push_str(&a.content);
                out.push_str("\n\n");
            }
        }
        out.push_str("</details>\n");
    }

    out
}

/// Format scoreboard for terminal display
pub fn format_scoreboard(scores: &[crate::history::history::ProviderScore]) -> String {
    use std::fmt::Write;

    if scores.is_empty() {
        return "No scores yet. Run some debates first!\n".to_string();
    }

    let mut out = String::new();
    out.push_str(&format!("\n{}\n", "═".repeat(70).bright_blue()));
    out.push_str(&format!("  📊 {}\n", "MODEL SCOREBOARD".bright_white().bold()));
    out.push_str(&format!("{}\n\n", "═".repeat(70).bright_blue()));

    // Header
    let _ = writeln!(
        out,
        "  {:<20} {:>6} {:>6} {:>6} {:>8} {:>8}",
        "Provider".bright_white().bold(),
        "Debates".bright_white().bold(),
        "Best".bright_white().bold(),
        "Found".bright_white().bold(),
        "Recv'd".bright_white().bold(),
        "Score".bright_white().bold(),
    );
    out.push_str(&format!("  {}\n", "─".repeat(62)));

    // Sorted by score descending
    let mut sorted: Vec<_> = scores.iter().collect();
    sorted.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    for s in &sorted {
        let score_str = format!("{:.1}", s.score);
        let score_colored = if s.score >= 80.0 {
            score_str.bright_green().to_string()
        } else if s.score >= 50.0 {
            score_str.bright_yellow().to_string()
        } else {
            score_str.red().to_string()
        };
        let _ = writeln!(
            out,
            "  {:<20} {:>6} {:>6} {:>6} {:>8} {:>8}",
            s.provider,
            s.total_debates,
            s.times_best,
            s.total_audit_issues_found,
            s.total_audit_issues_received,
            score_colored,
        );
    }

    out.push_str(&format!("\n  Score formula: (best×3 + (debates-received)×2) / max(debates×5, 1) × 100\n"));
    out.push_str(&format!("{}\n", "═".repeat(70).bright_blue()));
    out
}

/// Format history for terminal display
pub fn format_history(summaries: &[crate::history::history::DebateSummary]) -> String {
    use std::fmt::Write;

    if summaries.is_empty() {
        return "No debates in history yet.\n".to_string();
    }

    let mut out = String::new();
    out.push_str(&format!("\n{}\n", "═".repeat(70).bright_blue()));
    out.push_str(&format!("  📜 {}\n", "DEBATE HISTORY".bright_white().bold()));
    out.push_str(&format!("{}\n\n", "═".repeat(70).bright_blue()));

    for s in summaries {
        let synth = s.synthesis_provider.as_deref().unwrap_or("—");
        let _ = writeln!(
            out,
            "  {} #{:<4} {} | {} | {} providers | synth: {} | {:.1}s",
            s.timestamp.dimmed(),
            s.id,
            s.mode.bright_cyan(),
            s.prompt_preview.chars().take(50).collect::<String>(),
            s.num_providers,
            synth,
            s.total_secs,
        );
    }

    out.push_str(&format!("\n{}\n", "═".repeat(70).bright_blue()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_prompts_exist() {
        for mode in [
            DebateMode::Quick,
            DebateMode::General,
            DebateMode::Code,
            DebateMode::Debug,
            DebateMode::Adversarial,
        ] {
            let p = system_prompt(&mode);
            assert!(!p.is_empty());
        }
    }

    #[test]
    fn test_audit_system_not_empty() {
        assert!(!AUDIT_SYSTEM.is_empty());
        assert!(!SYNTHESIS_SYSTEM.is_empty());
    }

    #[test]
    fn test_synthesis_mentions_best() {
        assert!(SYNTHESIS_SYSTEM.contains("BEST:"));
    }

    #[test]
    fn test_format_empty_result() {
        let result = DebateResult {
            mode: "QUICK".into(),
            responses: vec![],
            audits: vec![],
            synthesis: None,
            total_secs: 0.0,
        };
        let text = format_text(&result);
        assert!(text.contains("Models consulted"));
    }

    #[test]
    fn test_parse_best_from_synthesis_found() {
        let content = "Here is the synthesis answer.\n\nBEST: kiro";
        let (best, cleaned) = parse_best_from_synthesis(content);
        assert_eq!(best, Some("kiro".to_string()));
        assert!(!cleaned.contains("BEST:"));
        assert!(cleaned.contains("synthesis answer"));
    }

    #[test]
    fn test_parse_best_from_synthesis_not_found() {
        let content = "Here is the synthesis answer without best marker.";
        let (best, cleaned) = parse_best_from_synthesis(content);
        assert_eq!(best, None);
        assert_eq!(cleaned, content);
    }

    #[test]
    fn test_parse_best_from_synthesis_with_trailing_whitespace() {
        let content = "Answer here.\n\n\nBEST: codex\n";
        let (best, cleaned) = parse_best_from_synthesis(content);
        assert_eq!(best, Some("codex".to_string()));
        assert_eq!(cleaned, "Answer here.");
    }

    #[test]
    fn test_format_scoreboard_empty() {
        let scores: Vec<crate::history::history::ProviderScore> = vec![];
        let text = format_scoreboard(&scores);
        assert!(text.contains("No scores yet"));
    }

    #[test]
    fn test_format_history_empty() {
        let summaries: Vec<crate::history::history::DebateSummary> = vec![];
        let text = format_history(&summaries);
        assert!(text.contains("No debates"));
    }
}
