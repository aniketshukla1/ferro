//! LLM provider: OpenAI-compatible chat completions with tools.
//! Covers Gemini (via generativelanguage OpenAI endpoint), OpenAI, Ollama,
//! and any OpenAI-compatible base URL. Key resolution order per call:
//! explicit flags > GEMINI_* > OPENAI_* > Ollama defaults.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("no API key: set GEMINI_API_KEY or OPENAI_API_KEY (or use Ollama with OLLAMA_MODEL)")]
    NoKey,
    #[error("transport: {0}")]
    Transport(String),
    #[error("bad response: {0}")]
    BadResponse(String),
    #[error("cancelled")]
    Cancelled,
    #[error("rate limited")]
    RateLimited { retry_after_ms: u64 },
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<WireToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireToolCall {
    pub id: String,
    #[serde(default)]
    pub r#type: String,
    pub function: WireFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone)]
pub struct Turn {
    pub content: Option<String>,
    pub tool_calls: Vec<WireToolCall>,
}

/// One streamed piece of an assistant message.
#[derive(Debug, Clone)]
pub struct ChatDelta {
    pub content_piece: Option<String>,
}

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[crate::ToolDef],
    ) -> Result<Turn, ProviderError>;

    /// Streaming variant. Default: single delta carrying the whole content.
    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[crate::ToolDef],
        tx: tokio::sync::mpsc::UnboundedSender<ChatDelta>,
    ) -> Result<Turn, ProviderError> {
        let turn = self.chat(messages, tools).await?;
        if let Some(c) = turn.content.clone() {
            let _ = tx.send(ChatDelta {
                content_piece: Some(c),
            });
        }
        Ok(turn)
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiCompat {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    client: reqwest::Client,
}

impl OpenAiCompat {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        // Connect 10 s, idle read 120 s (B0): streams must survive slow tokens.
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .read_timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            model: model.into(),
            client,
        }
    }

    /// Resolve from flags, then env. Gemini first (user's key), then OpenAI, then Ollama.
    pub fn from_env(
        api_key: Option<String>,
        base_url: Option<String>,
        model: Option<String>,
    ) -> Result<Self, ProviderError> {
        if let Some(k) = api_key.filter(|k| !k.is_empty()) {
            let base = base_url.unwrap_or_else(|| "https://api.openai.com/v1".into());
            let model = model.unwrap_or_else(|| "gpt-4o-mini".into());
            return Ok(Self::new(base, k, model));
        }
        if let Ok(k) = std::env::var("GEMINI_API_KEY") {
            if !k.is_empty() {
                let base = base_url.unwrap_or_else(|| {
                    "https://generativelanguage.googleapis.com/v1beta/openai".into()
                });
                let model = model
                    .or_else(|| std::env::var("GEMINI_MODEL").ok())
                    .unwrap_or_else(|| "gemini-2.0-flash".into());
                return Ok(Self::new(base, k, model));
            }
        }
        if let Ok(k) = std::env::var("OPENAI_API_KEY") {
            if !k.is_empty() {
                let base = base_url
                    .or_else(|| std::env::var("OPENAI_BASE_URL").ok())
                    .unwrap_or_else(|| "https://api.openai.com/v1".into());
                let model = model
                    .or_else(|| std::env::var("FERRO_MODEL").ok())
                    .unwrap_or_else(|| "gpt-4o-mini".into());
                return Ok(Self::new(base, k, model));
            }
        }
        if let Ok(m) = std::env::var("OLLAMA_MODEL") {
            let base = base_url.unwrap_or_else(|| "http://localhost:11434/v1".into());
            return Ok(Self::new(base, "ollama".to_string(), m));
        }
        Err(ProviderError::NoKey)
    }

    pub fn label(&self) -> String {
        format!("{} @ {}", self.model, self.base_url)
    }

    /// POST with retries on 408/409/429/5xx/529 and connection errors
    /// (max 2 retries, backoff with jitter, honors retry-after).
    async fn post_completions(
        &self,
        req: &ChatRequest<'_>,
    ) -> Result<reqwest::Response, ProviderError> {
        let url = format!("{}/chat/completions", self.base_url);
        let mut last_err = String::new();
        for attempt in 0..3u32 {
            let res = self
                .client
                .post(&url)
                .bearer_auth(&self.api_key)
                .header("Accept", "text/event-stream, application/json")
                .json(req)
                .send()
                .await;
            match res {
                Err(e) if e.is_connect() || e.is_timeout() => {
                    last_err = e.to_string();
                }
                Err(e) => return Err(ProviderError::Transport(e.to_string())),
                Ok(resp) => {
                    if resp.status().is_success() || !retryable(resp.status()) {
                        return Ok(resp);
                    }
                    let wait = retry_after_ms(resp.headers(), attempt);
                    last_err = format!(
                        "{}: {}",
                        resp.status(),
                        resp.text()
                            .await
                            .unwrap_or_default()
                            .chars()
                            .take(300)
                            .collect::<String>()
                    );
                    if attempt < 2 {
                        tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
                        continue;
                    }
                    return Err(ProviderError::BadResponse(last_err));
                }
            }
            if attempt < 2 {
                // Connection-level failure: back off before retrying.
                let wait = 500u64 << attempt.min(4);
                tokio::time::sleep(std::time::Duration::from_millis(
                    wait + rand::random::<u64>() % 250,
                ))
                .await;
            }
        }
        Err(ProviderError::Transport(last_err))
    }

    /// Single-shot completion without tools (commit messages, summaries).
    pub async fn complete_simple(&self, system: &str, user: &str) -> Result<String, ProviderError> {
        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: Some(system.to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".into(),
                content: Some(user.to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
        ];
        let req = ChatRequest {
            model: &self.model,
            messages: &messages,
            tools: vec![],
            tool_choice: None,
            stream: None,
        };
        let resp = self.post_completions(&req).await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let short: String = body.chars().take(300).collect();
            return Err(ProviderError::BadResponse(format!("{status}: {short}")));
        }
        let parsed: ChatResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::BadResponse(e.to_string()))?;
        Ok(parsed
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .unwrap_or_default()
            .trim()
            .to_string())
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    // Omitted when empty: OpenAI rejects `"tools": []` with `"tool_choice": "auto"` (D19).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

/// Retryable statuses: 408/409/429/5xx/529 (B0). Others fail immediately.
fn retryable(status: reqwest::StatusCode) -> bool {
    let c = status.as_u16();
    c == 408 || c == 409 || c == 429 || c == 529 || (500..600).contains(&c)
}

fn retry_after_ms(headers: &reqwest::header::HeaderMap, attempt: u32) -> u64 {
    if let Some(v) = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
    {
        if let Ok(s) = v.trim().parse::<u64>() {
            return (s * 1000).min(30_000);
        }
    }
    // Exponential backoff with jitter: 500ms, 1s (attempt is 0-based).
    let base = 500u64 << attempt.min(4);
    base + (rand::random::<u64>() % 250)
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: RespMessage,
}

#[derive(Deserialize)]
struct RespMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<WireToolCall>>,
}

#[async_trait]
impl LlmClient for OpenAiCompat {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[crate::ToolDef],
    ) -> Result<Turn, ProviderError> {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        self.chat_stream(messages, tools, tx).await
    }

    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[crate::ToolDef],
        tx: tokio::sync::mpsc::UnboundedSender<ChatDelta>,
    ) -> Result<Turn, ProviderError> {
        let wire_tools: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.params,
                    }
                })
            })
            .collect();
        let req = ChatRequest {
            model: &self.model,
            messages,
            tools: wire_tools,
            tool_choice: Some("auto"),
            stream: Some(true),
        };
        let resp = self.post_completions(&req).await?;
        // post_completions passes non-retryable errors through; reject them here.
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let short: String = body.chars().take(500).collect();
            return Err(ProviderError::BadResponse(format!("{status}: {short}")));
        }
        // Some servers ignore stream:true and return one JSON object.
        let ctype = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if ctype.contains("application/json") {
            let parsed: ChatResponse = resp
                .json()
                .await
                .map_err(|e| ProviderError::BadResponse(e.to_string()))?;
            let msg = parsed
                .choices
                .into_iter()
                .next()
                .ok_or_else(|| ProviderError::BadResponse("empty choices".into()))?
                .message;
            if let Some(c) = msg.content.clone() {
                let _ = tx.send(ChatDelta {
                    content_piece: Some(c),
                });
            }
            return Ok(Turn {
                content: msg.content,
                tool_calls: msg.tool_calls.unwrap_or_default(),
            });
        }
        let mut stream = resp.bytes_stream();
        let mut fold = SseFold::default();
        use tokio_stream::StreamExt as _;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ProviderError::Transport(e.to_string()))?;
            fold.feed_bytes(&chunk, &tx);
        }
        Ok(fold.finish())
    }
}

/// Raw streamed turn for the v2 adapter (usage + finish reason included).
#[derive(Debug, Default)]
pub struct RawTurn {
    pub content: Option<String>,
    pub tool_calls: Vec<WireToolCall>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub finish_reason: Option<String>,
    pub refusal: bool,
}

impl OpenAiCompat {
    /// Stream with caller-built tool JSON and usage reporting. `on_text`
    /// receives content pieces live.
    pub(crate) async fn stream_raw(
        &self,
        messages: &[ChatMessage],
        tools_json: Vec<serde_json::Value>,
        on_text: &tokio::sync::mpsc::UnboundedSender<String>,
    ) -> Result<RawTurn, ProviderError> {
        let req = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "tools": tools_json,
            "tool_choice": if tools_json.is_empty() { serde_json::Value::Null } else { serde_json::json!("auto") },
            "stream": true,
            "stream_options": {"include_usage": true},
        });
        // Omit empty tools entirely (D19): rebuild without the keys.
        let mut req = req;
        if tools_json.is_empty() {
            if let Some(m) = req.as_object_mut() {
                m.remove("tools");
                m.remove("tool_choice");
            }
        }
        let url = format!("{}/chat/completions", self.base_url);
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .header("Accept", "text/event-stream, application/json")
            .json(&req)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let short: String = body.chars().take(500).collect();
            return Err(ProviderError::BadResponse(format!("{status}: {short}")));
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut stream = resp.bytes_stream();
        let mut fold = SseFold::default();
        use tokio_stream::StreamExt as _;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ProviderError::Transport(e.to_string()))?;
            fold.feed_bytes(&chunk, &tx);
            while let Ok(d) = rx.try_recv() {
                if let Some(c) = d.content_piece {
                    let _ = on_text.send(c);
                }
            }
        }
        let content = if fold.content.is_empty() { None } else { Some(std::mem::take(&mut fold.content)) };
        let prompt_tokens = fold.prompt_tokens;
        let completion_tokens = fold.completion_tokens;
        let finish_reason = std::mem::take(&mut fold.finish_reason);
        let refusal = fold.refusal;
        let finished = fold.finish();
        Ok(RawTurn {
            content,
            tool_calls: finished.tool_calls,
            prompt_tokens,
            completion_tokens,
            finish_reason,
            refusal,
        })
    }
}
/// Incremental OpenAI-SSE folder: byte-buffered and line-split, so a
/// multi-byte character split across network chunks is never corrupted (D18).
/// Complete lines decode independently; source-invalid bytes go lossy (§ 5.2).
/// Also accumulates usage (`stream_options.include_usage`) and finish reason.
#[derive(Default)]
pub struct SseFold {
    pending: Vec<u8>,
    content: String,
    calls: std::collections::BTreeMap<u64, (Option<String>, String, String)>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub finish_reason: Option<String>,
    pub refusal: bool,
}

impl SseFold {
    pub fn feed_bytes(&mut self, chunk: &[u8], tx: &tokio::sync::mpsc::UnboundedSender<ChatDelta>) {
        self.pending.extend_from_slice(chunk);
        while let Some(ix) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=ix).collect();
            self.feed_line(&String::from_utf8_lossy(&line), tx);
        }
    }

    pub fn feed(&mut self, text: &str, tx: &tokio::sync::mpsc::UnboundedSender<ChatDelta>) {
        self.feed_bytes(text.as_bytes(), tx);
    }

    fn feed_line(&mut self, line: &str, tx: &tokio::sync::mpsc::UnboundedSender<ChatDelta>) {
        let line = line.trim();
        let Some(data) = line.strip_prefix("data:") else {
            return;
        };
        let data = data.trim();
        if data == "[DONE]" {
            return;
        }
        let v: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return,
        };
        let Some(delta) = v.pointer("/choices/0/delta") else {
            // Non-delta lines carry finish reasons and usage.
            if let Some(fr) = v
                .pointer("/choices/0/finish_reason")
                .and_then(|f| f.as_str())
            {
                if self.finish_reason.is_none() {
                    self.finish_reason = Some(fr.to_string());
                }
            }
            if let Some(u) = v.get("usage") {
                self.prompt_tokens += u.get("prompt_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
                self.completion_tokens +=
                    u.get("completion_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
            }
            return;
        };
        if let Some(c) = delta.get("content").and_then(|c| c.as_str()) {
            self.content.push_str(c);
            let _ = tx.send(ChatDelta {
                content_piece: Some(c.to_string()),
            });
        }
        if let Some(r) = delta.get("refusal").and_then(|r| r.as_str()) {
            if !r.is_empty() {
                self.refusal = true;
            }
        }
        if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tcs {
                let ixx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                let e = self
                    .calls
                    .entry(ixx)
                    .or_insert((None, String::new(), String::new()));
                if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
                    e.0 = Some(id.to_string());
                }
                if let Some(f) = tc.get("function") {
                    if let Some(n) = f.get("name").and_then(|n| n.as_str()) {
                        e.1.push_str(n);
                    }
                    if let Some(a) = f.get("arguments").and_then(|a| a.as_str()) {
                        e.2.push_str(a);
                    }
                }
            }
        }
    }

    pub fn finish(self) -> Turn {
        Turn {
            content: if self.content.is_empty() {
                None
            } else {
                Some(self.content)
            },
            tool_calls: self
                .calls
                .into_values()
                .map(|(id, name, arguments)| WireToolCall {
                    id: id.unwrap_or_default(),
                    r#type: "function".into(),
                    function: WireFunction { name, arguments },
                })
                .collect(),
        }
    }
}

/// Fold a complete SSE body (tests, non-streaming servers).
pub fn parse_sse_turn(
    body: &str,
    tx: &tokio::sync::mpsc::UnboundedSender<ChatDelta>,
) -> Result<Turn, ProviderError> {
    let mut fold = SseFold::default();
    fold.feed(body, tx);
    fold.feed("\n", tx);
    Ok(fold.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_folds_chunks_split_mid_line() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut fold = SseFold::default();
        // Feed in awkward splits: mid-JSON and mid-line (never inside a string).
        fold.feed("data: {\"choices\":[{\"delta\":{\"content\":\"Hel", &tx);
        fold.feed(
            "lo\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\" there\"}}]}\n",
            &tx,
        );
        fold.feed("data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"read_", &tx);
        fold.feed(
            "file\",\"arguments\":\"{\\\"path\\\":\\\"a\\\"}\"}}]}}]}\ndata: [DONE]\n",
            &tx,
        );
        drop(tx);
        let turn = fold.finish();
        assert_eq!(turn.content.as_deref(), Some("Hello there"));
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].function.name, "read_file");
        assert!(turn.tool_calls[0].function.arguments.contains("\"path\""));
        let mut got = String::new();
        let mut rx = rx;
        while let Ok(d) = rx.try_recv() {
            if let Some(c) = d.content_piece {
                got.push_str(&c);
            }
        }
        assert_eq!(got, "Hello there");
    }

    #[test]
    fn sse_ignores_garbage_lines() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let turn = parse_sse_turn(":ping\n\ndata: not json\n\ndata: [DONE]\n", &tx).unwrap();
        assert!(turn.content.is_none());
        assert!(turn.tool_calls.is_empty());
    }

    #[test]
    fn sse_split_multibyte_survives() {
        // "é" (0xC3 0xA9) split across chunks must not corrupt (D18).
        // The two sides below contain the literal raw bytes half each.
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut fold = SseFold::default();
        fold.feed_bytes(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"caf\xc3",
            &tx,
        );
        fold.feed_bytes(b"\xa9!\"}}]}\n", &tx);
        fold.feed_bytes(b"data: [DONE]\n", &tx);
        let turn = fold.finish();
        assert_eq!(turn.content.as_deref(), Some("café!"));
    }
}
