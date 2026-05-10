use crate::api::{ChatMessage, TokenUsage};
use crate::config::ProviderConfig;
use anyhow::Result;
use futures::StreamExt;
use std::time::Instant;
use tokio::io::AsyncWriteExt;

/// Result from a streaming call
pub struct StreamResponse {
    pub content: String,
    pub elapsed_secs: f64,
    pub tokens_used: Option<TokenUsage>,
    pub error: Option<String>,
}

/// Call a model with streaming enabled, printing tokens to stderr in real-time
pub async fn call_model_stream(
    provider: &ProviderConfig,
    messages: Vec<ChatMessage>,
    temperature: f32,
    max_tokens: u32,
    timeout_secs: u64,
) -> Result<StreamResponse> {
    let mtokens = provider.max_tokens.unwrap_or(max_tokens);
    let url = format!("{}/chat/completions", provider.base_url.trim_end_matches('/'));
    let start = Instant::now();

    let body = serde_json::json!({
        "model": provider.model,
        "messages": messages,
        "temperature": temperature,
        "max_tokens": mtokens,
        "stream": true,
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs + 30))
        .build()?;

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", provider.api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Ok(StreamResponse {
            content: String::new(),
            elapsed_secs: start.elapsed().as_secs_f64(),
            tokens_used: None,
            error: Some(format!("HTTP {}: {}", status, crate::api::truncate_str(&text, 200))),
        });
    }

    let mut content = String::new();
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let mut stderr = tokio::io::stderr();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let text = String::from_utf8_lossy(&chunk);
        buf.push_str(&text);

        // Process complete lines
        while let Some(newline_pos) = buf.find('\n') {
            let line = buf[..newline_pos].trim().to_string();
            buf = buf[newline_pos + 1..].to_string();

            if line.is_empty() {
                continue;
            }
            if line == "data: [DONE]" {
                continue;
            }

            if let Some(data) = line.strip_prefix("data: ") {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(data) {
                    if let Some(choices) = val.get("choices").and_then(|c| c.as_array()) {
                        if let Some(choice) = choices.first() {
                            let token = choice
                                .get("delta")
                                .and_then(|d| d.get("content"))
                                .and_then(|c| c.as_str());

                            if let Some(t) = token {
                                content.push_str(t);
                                // Print to stderr in real-time
                                let _ = stderr.write_all(t.as_bytes()).await;
                                let _ = stderr.flush().await;
                            }
                        }
                    }
                    // Check for usage in the stream
                    // (some providers send usage in the last chunk)
                }
            }
        }
    }

    let _ = stderr.write_all(b"\n").await;
    let _ = stderr.flush().await;

    let error = if content.is_empty() {
        Some("Empty stream response".into())
    } else {
        None
    };

    Ok(StreamResponse {
        content,
        elapsed_secs: start.elapsed().as_secs_f64(),
        tokens_used: None, // Streaming doesn't always provide token counts
        error,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_stream_module_exists() {
        // Basic smoke test that module compiles
        assert!(true);
    }
}
