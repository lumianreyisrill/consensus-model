#[cfg(feature = "history")]
pub mod history {
    use anyhow::{Context, Result};
    use rusqlite::{params, Connection};
    use serde::{Deserialize, Serialize};
    use std::path::Path;
    use std::sync::Mutex;

    use crate::api::ModelResponse;
    use crate::debate::DebateResult;

    /// Historical score for a provider
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ProviderScore {
        pub provider: String,
        pub total_debates: i64,
        pub times_best: i64,
        pub total_audit_issues_found: i64,
        pub total_audit_issues_received: i64,
        pub score: f64,
    }

    /// Summary row for `history` subcommand
    #[derive(Debug, Clone)]
    pub struct DebateSummary {
        pub id: i64,
        pub timestamp: String,
        pub mode: String,
        pub prompt_preview: String,
        pub total_secs: f64,
        pub num_providers: usize,
        pub synthesis_provider: Option<String>,
    }

    pub struct HistoryDb {
        conn: Mutex<Connection>,
    }

    impl HistoryDb {
        /// Open (or create) the SQLite database
        pub fn open(db_path: &Path) -> Result<Self> {
            if let Some(parent) = db_path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create DB dir: {}", parent.display()))?;
            }
            let conn = Connection::open(db_path)
                .with_context(|| format!("Failed to open DB: {}", db_path.display()))?;
            let db = Self {
                conn: Mutex::new(conn),
            };
            db.init_tables()?;
            Ok(db)
        }

        fn init_tables(&self) -> Result<()> {
            let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS debates (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    timestamp TEXT NOT NULL,
                    mode TEXT NOT NULL,
                    prompt_preview TEXT,
                    total_secs REAL,
                    num_providers INTEGER,
                    synthesis_provider TEXT
                );
                CREATE TABLE IF NOT EXISTS responses (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    debate_id INTEGER NOT NULL,
                    provider TEXT NOT NULL,
                    model TEXT NOT NULL,
                    content_chars INTEGER,
                    elapsed_secs REAL,
                    error TEXT,
                    tokens_prompt INTEGER,
                    tokens_completion INTEGER,
                    tokens_total INTEGER,
                    FOREIGN KEY (debate_id) REFERENCES debates(id)
                );
                CREATE TABLE IF NOT EXISTS audit_findings (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    debate_id INTEGER NOT NULL,
                    auditor TEXT NOT NULL,
                    target TEXT NOT NULL,
                    issues_found INTEGER DEFAULT 0,
                    FOREIGN KEY (debate_id) REFERENCES debates(id)
                );
                CREATE TABLE IF NOT EXISTS scores (
                    provider TEXT PRIMARY KEY,
                    total_debates INTEGER DEFAULT 0,
                    times_best INTEGER DEFAULT 0,
                    total_audit_issues_found INTEGER DEFAULT 0,
                    total_audit_issues_received INTEGER DEFAULT 0,
                    last_updated TEXT
                );"
            ).context("Failed to create tables")?;
            Ok(())
        }

        /// Log a completed debate
        pub fn log_debate(&self, result: &DebateResult, prompt: &str, best_provider: Option<&str>) -> Result<i64> {
            let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
            let timestamp = chrono_utc_now();
            let preview: String = prompt.chars().take(200).collect();
            let synth_provider = result.synthesis.as_ref().map(|s| s.provider.clone());

            conn.execute(
                "INSERT INTO debates (timestamp, mode, prompt_preview, total_secs, num_providers, synthesis_provider)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    timestamp,
                    result.mode,
                    preview,
                    result.total_secs,
                    result.responses.len() as i64,
                    synth_provider,
                ],
            ).context("Failed to insert debate")?;
            let debate_id = conn.last_insert_rowid();

            // Log individual responses
            for resp in &result.responses {
                let (tp, tc, tt) = match &resp.tokens_used {
                    Some(t) => (Some(t.prompt_tokens as i64), Some(t.completion_tokens as i64), Some(t.total_tokens as i64)),
                    None => (None, None, None),
                };
                conn.execute(
                    "INSERT INTO responses (debate_id, provider, model, content_chars, elapsed_secs, error, tokens_prompt, tokens_completion, tokens_total)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        debate_id,
                        resp.provider,
                        resp.model,
                        resp.content.len() as i64,
                        resp.elapsed_secs,
                        resp.error,
                        tp, tc, tt,
                    ],
                ).context("Failed to insert response")?;
            }

            // Count audit issues per target
            // Heuristic: count occurrences of patterns like "issue", "error", "flaw", "problem", "incorrect", "CRITICAL", "HIGH"
            let issue_patterns = ["issue", "error", "flaw", "problem", "incorrect", "critical", "high risk",
                                   "missing", "wrong", "bug", "vulnerability", "concern", "weakness"];

            // For each audit response, figure out which provider(s) it audited and count issues
            // An audit reviews ALL OTHER providers, so issues found by auditor apply to all targets
            for audit in &result.audits {
                if audit.error.is_some() {
                    continue;
                }
                let issues = count_issues(&audit.content, &issue_patterns);

                // This auditor audited all other valid providers
                let valid_targets: Vec<&ModelResponse> = result.responses.iter()
                    .filter(|r| r.error.is_none() && r.provider != audit.provider)
                    .collect();

                for target in &valid_targets {
                    conn.execute(
                        "INSERT INTO audit_findings (debate_id, auditor, target, issues_found)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![debate_id, audit.provider, target.provider, issues as i64],
                    ).context("Failed to insert audit finding")?;

                    // Update auditor's total_audit_issues_found
                    conn.execute(
                        "INSERT INTO scores (provider, total_debates, times_best, total_audit_issues_found, total_audit_issues_received, last_updated)
                         VALUES (?1, 0, 0, ?2, 0, ?3)
                         ON CONFLICT(provider) DO UPDATE SET
                             total_audit_issues_found = total_audit_issues_found + ?2,
                             last_updated = ?3",
                        params![audit.provider, issues as i64, timestamp],
                    ).context("Failed to update auditor score")?;

                    // Update target's total_audit_issues_received
                    conn.execute(
                        "INSERT INTO scores (provider, total_debates, times_best, total_audit_issues_found, total_audit_issues_received, last_updated)
                         VALUES (?1, 0, 0, 0, ?2, ?3)
                         ON CONFLICT(provider) DO UPDATE SET
                             total_audit_issues_received = total_audit_issues_received + ?2,
                             last_updated = ?3",
                        params![target.provider, issues as i64, timestamp],
                    ).context("Failed to update target score")?;
                }
            }

            // Update total_debates for all participating providers
            for resp in &result.responses {
                conn.execute(
                    "INSERT INTO scores (provider, total_debates, times_best, total_audit_issues_found, total_audit_issues_received, last_updated)
                     VALUES (?1, 1, 0, 0, 0, ?2)
                     ON CONFLICT(provider) DO UPDATE SET
                         total_debates = total_debates + 1,
                         last_updated = ?2",
                    params![resp.provider, timestamp],
                ).context("Failed to update provider debate count")?;
            }

            // Award times_best if best_provider detected
            if let Some(bp) = best_provider {
                conn.execute(
                    "INSERT INTO scores (provider, total_debates, times_best, total_audit_issues_found, total_audit_issues_received, last_updated)
                     VALUES (?1, 0, 1, 0, 0, ?2)
                     ON CONFLICT(provider) DO UPDATE SET
                         times_best = times_best + 1,
                         last_updated = ?2",
                    params![bp, timestamp],
                ).context("Failed to update best provider")?;
            }

            Ok(debate_id)
        }

        /// Get all scores computed
        pub fn get_scores(&self) -> Result<Vec<ProviderScore>> {
            let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT provider, total_debates, times_best, total_audit_issues_found, total_audit_issues_received
                 FROM scores ORDER BY provider"
            ).context("Failed to prepare scores query")?;

            let scores = stmt.query_map([], |row| {
                let provider: String = row.get(0)?;
                let total_debates: i64 = row.get(1)?;
                let times_best: i64 = row.get(2)?;
                let found: i64 = row.get(3)?;
                let received: i64 = row.get(4)?;
                let denom = std::cmp::max(total_debates * 5, 1) as f64;
                let score = ((times_best * 3 + (total_debates - received).max(0) * 2) as f64 / denom) * 100.0;
                Ok(ProviderScore {
                    provider,
                    total_debates,
                    times_best,
                    total_audit_issues_found: found,
                    total_audit_issues_received: received,
                    score,
                })
            })
            .context("Failed to query scores")?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to collect scores")?;

            Ok(scores)
        }

        /// Get debate history summaries
        pub fn get_history(&self, limit: usize, provider_filter: Option<&str>) -> Result<Vec<DebateSummary>> {
            let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("Lock error: {}", e))?;

            let (sql, param_values): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match provider_filter {
                Some(prov) => {
                    let sql = "SELECT DISTINCT d.id, d.timestamp, d.mode, d.prompt_preview, d.total_secs, d.num_providers, d.synthesis_provider
                        FROM debates d
                        JOIN responses r ON r.debate_id = d.id
                        WHERE r.provider = ?1
                        ORDER BY d.id DESC LIMIT ?2".to_string();
                    (sql, vec![Box::new(prov.to_string()), Box::new(limit as i64)])
                }
                None => {
                    let sql = "SELECT id, timestamp, mode, prompt_preview, total_secs, num_providers, synthesis_provider
                        FROM debates ORDER BY id DESC LIMIT ?1".to_string();
                    (sql, vec![Box::new(limit as i64)])
                }
            };

            let mut stmt = conn.prepare(&sql).context("Failed to prepare history query")?;
            let param_refs: Vec<&dyn rusqlite::types::ToSql> = param_values.iter().map(|v| v.as_ref()).collect();
            let summaries = stmt.query_map(param_refs.as_slice(), |row| {
                Ok(DebateSummary {
                    id: row.get(0)?,
                    timestamp: row.get(1)?,
                    mode: row.get(2)?,
                    prompt_preview: row.get(3)?,
                    total_secs: row.get(4)?,
                    num_providers: row.get::<_, i64>(5)? as usize,
                    synthesis_provider: row.get(6)?,
                })
            })
            .context("Failed to query history")?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to collect history")?;

            Ok(summaries)
        }

        /// Get the provider with highest score, if any history exists
        pub fn get_best_provider(&self) -> Result<Option<String>> {
            let scores = self.get_scores()?;
            if scores.is_empty() {
                return Ok(None);
            }
            let best = scores.iter().max_by(|a, b| {
                a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal)
            });
            Ok(best.map(|s| s.provider.clone()))
        }
    }

    /// Count issues in audit text by pattern matching
    fn count_issues(text: &str, patterns: &[&str]) -> usize {
        let lower = text.to_lowercase();
        let mut count = 0usize;
        // Count by looking at pattern matches — each "## Issue", numbered item, or bullet counts as one
        // Also count by key phrases
        for line in lower.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("## ")
                || trimmed.starts_with("### ")
                || (trimmed.starts_with(|c: char| c.is_ascii_digit()) && trimmed.contains('.'))
                || trimmed.starts_with("- ")
                || trimmed.starts_with("* ")
            {
                for pat in patterns {
                    if trimmed.contains(pat) {
                        count += 1;
                        break;
                    }
                }
            }
        }
        // At minimum, if we have content and no structured issues found, check for key terms
        if count == 0 && !text.trim().is_empty() {
            for pat in patterns {
                if lower.contains(pat) {
                    count += 1;
                    break;
                }
            }
        }
        count
    }

    /// Simple UTC timestamp without adding chrono dependency
    fn chrono_utc_now() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let dur = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        let secs = dur.as_secs();
        // Convert to approximate date string
        let days = secs / 86400;
        let time_of_day = secs % 86400;
        let hours = time_of_day / 3600;
        let minutes = (time_of_day % 3600) / 60;
        let seconds = time_of_day % 60;
        // Simple days since epoch to Y-M-D
        let (y, m, d) = days_to_ymd(days);
        format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, m, d, hours, minutes, seconds)
    }

    fn days_to_ymd(days: u64) -> (i32, u32, u32) {
        let mut y = 1970i32;
        let mut remaining = days;
        loop {
            let year_days = if is_leap(y) { 366 } else { 365 };
            if remaining < year_days {
                break;
            }
            remaining -= year_days;
            y += 1;
        }
        let leap = is_leap(y);
        let month_days: &[u32] = if leap {
            &[31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
        } else {
            &[31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
        };
        let mut m = 1u32;
        for &md in month_days {
            if remaining < md as u64 {
                break;
            }
            remaining -= md as u64;
            m += 1;
        }
        (y, m, remaining as u32 + 1)
    }

    fn is_leap(y: i32) -> bool {
        (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_count_issues_with_patterns() {
            let text = "## Issue 1: Bug\nThere is a critical error in the code.\n## Issue 2: Missing validation\nThe input is not validated.";
            let patterns = ["issue", "error", "bug", "missing", "critical"];
            assert_eq!(count_issues(text, &patterns), 2);
        }

        #[test]
        fn test_count_issues_minimal() {
            let text = "The response looks correct overall with minor concerns.";
            let patterns = ["issue", "error", "bug", "concern"];
            assert_eq!(count_issues(text, &patterns), 1);
        }

        #[test]
        fn test_count_issues_empty() {
            let text = "Everything looks perfect and accurate.";
            let patterns = ["issue", "error", "bug"];
            assert_eq!(count_issues(text, &patterns), 0);
        }

        #[test]
        fn test_days_to_ymd() {
            // Jan 1, 1970 = day 0
            assert_eq!(days_to_ymd(0), (1970, 1, 1));
            // Jan 2, 1970 = day 1
            assert_eq!(days_to_ymd(1), (1970, 1, 2));
        }

        #[test]
        fn test_score_formula() {
            // 10 debates, 5 times best, 10 issues received
            let total_debates = 10i64;
            let times_best = 5i64;
            let received = 10i64;
            let denom = std::cmp::max(total_debates * 5, 1) as f64;
            let score = ((times_best * 3 + (total_debates - received).max(0) * 2) as f64 / denom) * 100.0;
            // (15 + 0) / 50 * 100 = 30.0
            assert!((score - 30.0).abs() < 0.01);
        }

        #[test]
        fn test_history_db_open_and_scores() -> Result<()> {
            let db_path = std::path::PathBuf::from("/tmp/consensus_test_history.db");
            let db = HistoryDb::open(&db_path)?;
            let scores = db.get_scores()?;
            assert!(scores.is_empty());
            let history = db.get_history(10, None)?;
            assert!(history.is_empty());
            std::fs::remove_file(&db_path).ok();
            Ok(())
        }
    }
}

// Stub module when history feature is disabled
#[cfg(not(feature = "history"))]
pub mod history {
    use anyhow::Result;
    use crate::debate::DebateResult;

    pub struct HistoryDb;
    pub struct ProviderScore { pub provider: String, pub score: f64, pub total_debates: i64, pub times_best: i64, pub total_audit_issues_found: i64, pub total_audit_issues_received: i64 }
    pub struct DebateSummary { pub id: i64, pub timestamp: String, pub mode: String, pub prompt_preview: String, pub total_secs: f64, pub num_providers: usize, pub synthesis_provider: Option<String> }

    impl HistoryDb {
        pub fn open(_path: &std::path::Path) -> Result<Self> {
            anyhow::bail!("History feature not compiled. Rebuild with `--features history`")
        }
        pub fn log_debate(&self, _result: &DebateResult, _prompt: &str, _best: Option<&str>) -> Result<i64> {
            anyhow::bail!("History feature not compiled")
        }
        pub fn get_scores(&self) -> Result<Vec<ProviderScore>> {
            anyhow::bail!("History feature not compiled")
        }
        pub fn get_history(&self, _limit: usize, _provider: Option<&str>) -> Result<Vec<DebateSummary>> {
            anyhow::bail!("History feature not compiled")
        }
        pub fn get_best_provider(&self) -> Result<Option<String>> {
            Ok(None)
        }
    }
}
