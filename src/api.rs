use crate::config::ProviderConfig;
use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// A response from a single model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelResponse {
    pub provider: String,
    pub label: String,
    pub model: String,
    pub content: String,
    pub elapsed_secs: f64,
    pub error: Option<String>,
    pub tokens_used: Option<TokenUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// OpenAI-compatible API client
pub struct ApiClient {
    http: Client,
    #[allow(dead_code)]
    timeout: Duration,
    max_retries: u32,
}

/// OpenAI chat completion request
#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
    max_tokens: u32,
    stream: bool,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// OpenAI chat completion response
#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    usage: Option<UsageResponse>,
}

#[derive(Deserialize)]
struct Choice {
    message: MessageContent,
}

#[derive(Deserialize)]
struct MessageContent {
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
}

#[derive(Deserialize)]
struct UsageResponse {
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
    total_tokens: Option<u32>,
}

impl ApiClient {
    pub fn new(timeout_secs: u64, max_retries: u32) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(timeout_secs + 30)) // client timeout > API timeout
            .build()
            .context("Failed to build HTTP client")?;
        Ok(Self {
            http,
            timeout: Duration::from_secs(timeout_secs),
            max_retries,
        })
    }

    /// Call a single model with retry logic
    pub async fn call_model(
        &self,
        provider: &ProviderConfig,
        messages: Vec<ChatMessage>,
        temperature: f32,
        max_tokens: u32,
    ) -> ModelResponse {
        let start = Instant::now();
        let display_name = provider.display_name().to_string();
        let mtokens = provider.max_tokens.unwrap_or(max_tokens);
        let mut last_error = None;

        for attempt in 0..=self.max_retries {
            if attempt > 0 {
                let delay = Duration::from_millis(500 * 2u64.pow(attempt - 1));
                tokio::time::sleep(delay).await;
            }

            let req = ChatRequest {
                model: provider.model.clone(),
                messages: messages.clone(),
                temperature,
                max_tokens: mtokens,
                stream: false,
            };

            let url = format!("{}/chat/completions", provider.base_url.trim_end_matches('/'));

            match self
                .http
                .post(&url)
                .header("Authorization", format!("Bearer {}", provider.api_key))
                .header("Content-Type", "application/json")
                .json(&req)
                .send()
                .await
            {
                Ok(resp) => {
                    let elapsed = start.elapsed().as_secs_f64();
                    let status = resp.status();

                    if !status.is_success() {
                        let text = resp.text().await.unwrap_or_default();
                        let err = format!("HTTP {}: {}", status, truncate_str(&text, 200));
                        // Don't retry on client errors (4xx)
                        if status.is_client_error() && status.as_u16() != 429 {
                            return ModelResponse {
                                provider: provider.name.clone(),
                                label: display_name,
                                model: provider.model.clone(),
                                content: String::new(),
                                elapsed_secs: elapsed,
                                error: Some(err),
                                tokens_used: None,
                            };
                        }
                        last_error = Some(err);
                        continue;
                    }

                    match resp.text().await {
                        Ok(text) => match serde_json::from_str::<ChatResponse>(&text) {
                            Ok(chat_resp) => {
                                let choice = chat_resp.choices.first();
                                let content = choice
                                    .and_then(|c| {
                                        c.message
                                            .content
                                            .as_deref()
                                            .filter(|s| !s.is_empty())
                                            .or(c.message.reasoning_content.as_deref())
                                    })
                                    .unwrap_or("")
                                    .to_string();

                                let tokens = chat_resp.usage.map(|u| TokenUsage {
                                    prompt_tokens: u.prompt_tokens.unwrap_or(0),
                                    completion_tokens: u.completion_tokens.unwrap_or(0),
                                    total_tokens: u.total_tokens.unwrap_or(0),
                                });

                                return ModelResponse {
                                    provider: provider.name.clone(),
                                    label: display_name,
                                    model: provider.model.clone(),
                                    content,
                                    elapsed_secs: elapsed,
                                    error: None,
                                    tokens_used: tokens,
                                };
                            }
                            Err(e) => {
                                let err = format!("JSON parse error: {} — raw: {}", e, truncate_str(&text, 200));
                                // If SSE streaming response, try to parse
                                if text.starts_with("data:") {
                                    if let Some(content) = parse_sse_response(&text) {
                                        return ModelResponse {
                                            provider: provider.name.clone(),
                                            label: display_name,
                                            model: provider.model.clone(),
                                            content,
                                            elapsed_secs: start.elapsed().as_secs_f64(),
                                            error: None,
                                            tokens_used: None,
                                        };
                                    }
                                }
                                last_error = Some(err);
                                continue;
                            }
                        },
                        Err(e) => {
                            last_error = Some(format!("Body read error: {}", e));
                            continue;
                        }
                    }
                }
                Err(e) => {
                    last_error = Some(format!("Request error: {}", e));
                    continue;
                }
            }
        }

        ModelResponse {
            provider: provider.name.clone(),
            label: display_name,
            model: provider.model.clone(),
            content: String::new(),
            elapsed_secs: start.elapsed().as_secs_f64(),
            error: last_error.or_else(|| Some("Unknown error".into())),
            tokens_used: None,
        }
    }
}

/// Parse SSE streaming response (data: {...}\n data: [DONE])
fn parse_sse_response(text: &str) -> Option<String> {
    let mut content = String::new();
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with("data: ") || line == "data: [DONE]" {
            continue;
        }
        let json_str = &line[6..]; // skip "data: "
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
            // Standard OpenAI SSE format
            if let Some(choices) = val.get("choices").and_then(|c| c.as_array()) {
                if let Some(choice) = choices.first() {
                    // delta.content (streaming) or message.content (non-stream)
                    let text = choice
                        .get("delta")
                        .or_else(|| choice.get("message"))
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_str());
                    if let Some(t) = text {
                        content.push_str(t);
                    }
                }
            }
        }
    }
    if content.is_empty() {
        None
    } else {
        Some(content)
    }
}

pub fn truncate_str(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        let mut end = max_len;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sse() {
        let input = "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\ndata: [DONE]\n";
        assert_eq!(parse_sse_response(input), Some("Hello world".into()));
    }

    #[test]
    fn test_parse_sse_empty() {
        assert_eq!(parse_sse_response("data: [DONE]"), None);
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate_str("hello", 3), "hel");
        assert_eq!(truncate_str("hi", 5), "hi");
    }
}
