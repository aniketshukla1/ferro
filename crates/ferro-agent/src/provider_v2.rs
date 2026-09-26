//! LLM provider v2 (B5, Appendix B): streaming turns with tool calls,
//! usage accounting, and cancellation. Native Anthropic plus an
//! OpenAI-compatible adapter (OpenAI, Gemini, Ollama, custom).
//! Recorded-SSE tests run against a local mock — no network in CI.

use async_trait::async_trait;
use std::sync::Arc;

use super::provider::ProviderError;

impl ProviderError {
    fn retryable(msg: String) -> Self {
        ProviderError::Transport(format!("retryable: {msg}"))
    }
}

#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StopReason {
    #[default]
    EndTurn,
    ToolUse,
    MaxTokens,
    Refusal,
    StopSequence,
    PauseTurn,
}

#[derive(Debug, Clone)]
pub enum LlmEvent {
    Text(String),
    Thinking(String),
    ToolStart { id: String, name: String },
    ToolDelta { id: String, json: String },
    Usage(Usage),
}

#[derive(Debug, Clone)]
pub struct ToolCallV2 {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
    pub input_raw: String,
    pub input_ok: bool,
}

#[derive(Debug, Clone)]
pub struct TurnOutcome {
    pub text: String,
    pub thinking: Vec<ThinkingBlock>,
    pub calls: Vec<ToolCallV2>,
    /// Assistant blocks in stream order, for exact echo on continuation.
    pub blocks: Vec<TurnBlock>,
    pub stop: StopReason,
    pub usage: Usage,
}

#[derive(Debug, Clone)]
pub enum TurnBlock {
    Text(String),
    Thinking(ThinkingBlock),
    ToolUse(ToolCallV2),
}

#[derive(Debug, Clone)]
pub struct ThinkingBlock {
    pub text: String,
    pub signature: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgRole {
    User,
    Assistant,
}

#[derive(Debug, Clone)]
pub enum MsgBlock {
    Text(String),
    Thinking {
        text: String,
        signature: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
}

#[derive(Debug, Clone)]
pub struct Msg {
    pub role: MsgRole,
    pub blocks: Vec<MsgBlock>,
    /// Cache breakpoint after this message (large stable context).
    pub cache: bool,
}

#[derive(Debug, Clone)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    /// Strict tools (report_finding): `strict: true` on the wire.
    pub strict: bool,
}

pub struct ChatReq<'a> {
    pub system: &'a str,
    pub messages: &'a [Msg],
    pub tools: &'a [ToolSchema],
    pub max_tokens: u32,
    /// low|medium|high|xhigh|max → output_config.
    pub effort: Option<&'a str>,
    pub stop: tokio_util::sync::CancellationToken,
}

#[async_trait]
pub trait LlmClientV2: Send + Sync {
    fn provider_id(&self) -> &'static str;
    fn model_id(&self) -> &str;
    async fn stream_turn(
        &self,
        req: ChatReq<'_>,
        tx: tokio::sync::mpsc::UnboundedSender<LlmEvent>,
    ) -> Result<TurnOutcome, ProviderError>;
}

// -- provider selection ---------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Anthropic,
    OpenAI,
    Gemini,
    Ollama,
    Compat,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::OpenAI => "openai",
            ProviderKind::Gemini => "gemini",
            ProviderKind::Ollama => "ollama",
            ProviderKind::Compat => "openai-compatible",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "Anthropic",
            ProviderKind::OpenAI => "OpenAI",
            ProviderKind::Gemini => "Gemini",
            ProviderKind::Ollama => "Ollama",
            ProviderKind::Compat => "OpenAI-compatible",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "claude-opus-5",
            ProviderKind::OpenAI => "gpt-4o-mini",
            ProviderKind::Gemini => "gemini-2.0-flash",
            ProviderKind::Ollama => "",
            ProviderKind::Compat => "",
        }
    }
}

/// Resolved provider for status reporting and client construction.
#[derive(Debug, Clone)]
pub struct ProviderSpec {
    pub kind: ProviderKind,
    pub model: String,
    pub base_url: String,
}

/// Auto order: Anthropic → OpenAI → Gemini → Ollama (spec §B5). Explicit
/// model/base_url overrides select `compat` when no env matches... — no:
/// overrides refine the auto pick; an explicit base URL alone means compat.
pub fn resolve_provider(
    model: Option<String>,
    base_url: Option<String>,
) -> Result<ProviderSpec, ProviderError> {
    if let Some(url) = base_url.clone().filter(|u| !u.is_empty()) {
        return Ok(ProviderSpec {
            kind: ProviderKind::Compat,
            model: model.unwrap_or_default(),
            base_url: url,
        });
    }
    if std::env::var("ANTHROPIC_API_KEY")
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false)
        || std::env::var("ANTHROPIC_AUTH_TOKEN")
            .map(|k| !k.trim().is_empty())
            .unwrap_or(false)
    {
        return Ok(ProviderSpec {
            kind: ProviderKind::Anthropic,
            model: model.unwrap_or_else(|| "claude-opus-5".into()),
            base_url: std::env::var("ANTHROPIC_BASE_URL").unwrap_or_default(),
        });
    }
    if std::env::var("OPENAI_API_KEY")
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false)
    {
        return Ok(ProviderSpec {
            kind: ProviderKind::OpenAI,
            model: model.unwrap_or_else(|| "gpt-4o-mini".into()),
            base_url: std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".into()),
        });
    }
    if std::env::var("GEMINI_API_KEY")
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false)
    {
        return Ok(ProviderSpec {
            kind: ProviderKind::Gemini,
            model: model.unwrap_or_else(|| "gemini-2.0-flash".into()),
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai".into(),
        });
    }
    if let Ok(m) = std::env::var("OLLAMA_MODEL") {
        if !m.trim().is_empty() {
            return Ok(ProviderSpec {
                kind: ProviderKind::Ollama,
                model: model.unwrap_or(m),
                base_url: "http://localhost:11434/v1".into(),
            });
        }
    }
    Err(ProviderError::NoKey)
}

/// Status rows for GET /ai/status.
pub fn provider_status() -> Vec<serde_json::Value> {
    let has = |k: &str| {
        std::env::var(k)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    };
    vec![
        serde_json::json!({"id": "anthropic", "label": "Anthropic", "configured": has("ANTHROPIC_API_KEY") || has("ANTHROPIC_AUTH_TOKEN"), "defaultModel": "claude-opus-5"}),
        serde_json::json!({"id": "openai", "label": "OpenAI", "configured": has("OPENAI_API_KEY")}),
        serde_json::json!({"id": "gemini", "label": "Gemini", "configured": has("GEMINI_API_KEY")}),
        serde_json::json!({"id": "ollama", "label": "Ollama", "configured": has("OLLAMA_MODEL")}),
        serde_json::json!({"id": "openai-compatible", "label": "OpenAI-compatible", "configured": false}),
    ]
}

// -- native Anthropic -------------------------------------------------------------

pub struct Anthropic {
    base_url: String,
    model: String,
    key: Option<String>,
    bearer: Option<String>,
    client: reqwest::Client,
}

impl Anthropic {
    pub fn new(model: String, base_url: String) -> Result<Self, ProviderError> {
        let key = std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty());
        let bearer = std::env::var("ANTHROPIC_AUTH_TOKEN")
            .ok()
            .filter(|k| !k.trim().is_empty());
        if key.is_none() && bearer.is_none() && ant_token().is_none() {
            return Err(ProviderError::NoKey);
        }
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .read_timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Ok(Self {
            base_url: if base_url.is_empty() {
                "https://api.anthropic.com".into()
            } else {
                base_url.trim_end_matches('/').into()
            },
            model,
            key,
            bearer,
            client,
        })
    }

    fn headers(&self, refreshed: Option<String>) -> Vec<(String, String)> {
        let mut h = vec![
            ("anthropic-version".into(), "2023-06-01".into()),
            ("content-type".into(), "application/json".into()),
            (
                "anthropic-beta".into(),
                "server-side-fallback-2026-07-01".into(),
            ),
        ];
        if let Some(b) = refreshed.or_else(|| self.bearer.clone()) {
            h.push(("authorization".into(), format!("Bearer {b}")));
            h.push(("anthropic-beta".into(), "oauth-2025-04-20".into()));
        } else if let Some(k) = self.key.clone() {
            h.push(("x-api-key".into(), k));
        } else if let Some(t) = ant_token() {
            h.push(("authorization".into(), format!("Bearer {t}")));
            h.push(("anthropic-beta".into(), "oauth-2025-04-20".into()));
        }
        h
    }

    fn body(&self, req: &ChatReq<'_>) -> serde_json::Value {
        let system: Vec<serde_json::Value> = vec![serde_json::json!({
            "type": "text",
            "text": req.system,
            "cache_control": {"type": "ephemeral"},
        })];
        let messages: Vec<serde_json::Value> = req
            .messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    MsgRole::User => "user",
                    MsgRole::Assistant => "assistant",
                };
                let mut blocks = Vec::new();
                for b in &m.blocks {
                    blocks.push(match b {
                        MsgBlock::Text(t) => serde_json::json!({"type": "text", "text": t}),
                        MsgBlock::Thinking { text, signature } => serde_json::json!({
                            "type": "thinking", "thinking": text, "signature": signature,
                        }),
                        MsgBlock::ToolUse { id, name, input } => serde_json::json!({
                            "type": "tool_use", "id": id, "name": name, "input": input,
                        }),
                        MsgBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => serde_json::json!({
                            "type": "tool_result", "tool_use_id": tool_use_id,
                            "content": content, "is_error": is_error,
                        }),
                    });
                }
                if m.cache {
                    if let Some(last) = blocks.last_mut() {
                        last["cache_control"] = serde_json::json!({"type": "ephemeral"});
                    }
                }
                serde_json::json!({"role": role, "content": blocks})
            })
            .collect();
        let tools: Vec<serde_json::Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut o = serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                    "eager_input_streaming": true,
                });
                if t.strict {
                    o["strict"] = true.into();
                }
                o
            })
            .collect();
        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": req.max_tokens,
            "stream": true,
            "system": system,
            "messages": messages,
            "tool_choice": {"type": "auto"},
            "fallbacks": "default",
        });
        if !tools.is_empty() {
            body["tools"] = tools.into();
        }
        if let Some(e) = req.effort {
            body["output_config"] = serde_json::json!({"effort": e});
        }
        body
    }

    async fn post(
        &self,
        body: &serde_json::Value,
        refreshed: Option<String>,
    ) -> Result<reqwest::Response, ProviderError> {
        let url = format!("{}/v1/messages", self.base_url);
        let mut req = self.client.post(&url);
        for (k, v) in self.headers(refreshed) {
            req = req.header(k, v);
        }
        let resp = req.json(body).send().await.map_err(|e| {
            if e.is_connect() || e.is_timeout() {
                ProviderError::retryable(e.to_string())
            } else {
                ProviderError::Transport(e.to_string())
            }
        })?;
        let status = resp.status().as_u16();
        match status {
            200 => Ok(resp),
            400 | 402 | 403 | 404 | 413 => Err(ProviderError::BadResponse(format!("{status}"))),
            401 => Err(ProviderError::Transport("unauthorized".into())),
            429 => Err(ProviderError::RateLimited {
                retry_after_ms: resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(|s| s * 1000)
                    .unwrap_or(60_000),
            }),
            500 | 529 => Err(ProviderError::retryable(format!("{status}"))),
            _ if (500..600).contains(&status) || status == 408 || status == 409 => {
                Err(ProviderError::retryable(format!("{status}")))
            }
            _ => Err(ProviderError::BadResponse(format!("{status}"))),
        }
    }
}

/// Short-lived `ant` CLI token (refreshed on 401).
fn ant_token() -> Option<String> {
    let out = std::process::Command::new("ant")
        .args(["auth", "print-credentials", "--access-token"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!t.is_empty()).then_some(t)
}

#[derive(Default)]
struct AnthropicFold {
    text: String,
    thinking: String,
    blocks: Vec<BlockInFlight>,
    usage: Usage,
    stop: StopReason,
    saw_stop: bool,
    refusal: bool,
}

struct BlockInFlight {
    index: u64,
    kind: BlockKind,
    id: String,
    name: String,
    json: String,
    text: String,
    thinking: String,
    signature: String,
}

#[derive(PartialEq, Eq)]
enum BlockKind {
    Text,
    Thinking,
    Tool,
}

impl AnthropicFold {
    fn block(&mut self, index: u64) -> &mut BlockInFlight {
        if !self.blocks.iter().any(|b| b.index == index) {
            self.blocks.push(BlockInFlight {
                index,
                kind: BlockKind::Text,
                id: String::new(),
                name: String::new(),
                json: String::new(),
                text: String::new(),
                thinking: String::new(),
                signature: String::new(),
            });
        }
        self.blocks.iter_mut().find(|b| b.index == index).unwrap()
    }

    fn feed_event(
        &mut self,
        v: &serde_json::Value,
        tx: &tokio::sync::mpsc::UnboundedSender<LlmEvent>,
    ) -> Result<(), ProviderError> {
        let t = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
        match t {
            "message_start" => {
                if let Some(u) = v.pointer("/message/usage") {
                    self.usage.input += u.get("input_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
                    self.usage.cache_write += u
                        .get("cache_creation_input_tokens")
                        .and_then(|n| n.as_u64())
                        .unwrap_or(0);
                    self.usage.cache_read += u
                        .get("cache_read_input_tokens")
                        .and_then(|n| n.as_u64())
                        .unwrap_or(0);
                }
            }
            "content_block_start" => {
                let index = v.get("index").and_then(|n| n.as_u64()).unwrap_or(0);
                let b = v
                    .get("content_block")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let kind = b.get("type").and_then(|x| x.as_str()).unwrap_or("");
                let blk = self.block(index);
                match kind {
                    "tool_use" => {
                        blk.kind = BlockKind::Tool;
                        blk.id = b.get("id").and_then(|x| x.as_str()).unwrap_or("").into();
                        blk.name = b.get("name").and_then(|x| x.as_str()).unwrap_or("").into();
                        let _ = tx.send(LlmEvent::ToolStart {
                            id: blk.id.clone(),
                            name: blk.name.clone(),
                        });
                    }
                    "thinking" => blk.kind = BlockKind::Thinking,
                    _ => blk.kind = BlockKind::Text,
                }
            }
            "content_block_delta" => {
                let index = v.get("index").and_then(|n| n.as_u64()).unwrap_or(0);
                let d = v.get("delta").cloned().unwrap_or(serde_json::Value::Null);
                let kind = d.get("type").and_then(|x| x.as_str()).unwrap_or("");
                match kind {
                    "text_delta" => {
                        let s = d.get("text").and_then(|x| x.as_str()).unwrap_or("");
                        self.text.push_str(s);
                        let blk = self.block(index);
                        blk.kind = BlockKind::Text;
                        blk.text.push_str(s);
                        let _ = tx.send(LlmEvent::Text(s.to_string()));
                    }
                    "thinking_delta" => {
                        let s = d.get("thinking").and_then(|x| x.as_str()).unwrap_or("");
                        self.thinking.push_str(s);
                        let blk = self.block(index);
                        blk.kind = BlockKind::Thinking;
                        blk.thinking.push_str(s);
                        let _ = tx.send(LlmEvent::Thinking(s.to_string()));
                    }
                    "signature_delta" => {
                        let s = d.get("signature").and_then(|x| x.as_str()).unwrap_or("");
                        let blk = self.block(index);
                        blk.signature.push_str(s);
                    }
                    "input_json_delta" => {
                        let s = d.get("partial_json").and_then(|x| x.as_str()).unwrap_or("");
                        let blk = self.block(index);
                        blk.kind = BlockKind::Tool;
                        blk.json.push_str(s);
                        let _ = tx.send(LlmEvent::ToolDelta {
                            id: blk.id.clone(),
                            json: s.to_string(),
                        });
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(u) = v.get("usage") {
                    self.usage.output +=
                        u.get("output_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
                }
                if let Some(stop) = v.pointer("/delta/stop_reason").and_then(|x| x.as_str()) {
                    self.saw_stop = true;
                    self.stop = match stop {
                        "tool_use" => StopReason::ToolUse,
                        "max_tokens" => StopReason::MaxTokens,
                        "refusal" => StopReason::Refusal,
                        "stop_sequence" => StopReason::StopSequence,
                        "pause_turn" => StopReason::PauseTurn,
                        _ => StopReason::EndTurn,
                    };
                }
                if v.pointer("/delta/stop_details").is_some() {
                    // Refusal details surface here; the stop_reason above
                    // already records it. Never run tools on refusals.
                    self.refusal = self.stop == StopReason::Refusal;
                }
            }
            "ping" | "message_stop" | "content_block_stop" => {}
            "error" => {
                let kind = v
                    .pointer("/error/type")
                    .and_then(|x| x.as_str())
                    .unwrap_or("");
                if kind == "overloaded_error" {
                    return Err(ProviderError::retryable("overloaded_error".into()));
                }
                return Err(ProviderError::Transport(format!("stream error: {kind}")));
            }
            _ => {}
        }
        let _ = tx.send(LlmEvent::Usage(self.usage.clone()));
        Ok(())
    }

    fn finish(mut self) -> TurnOutcome {
        let mut calls = Vec::new();
        let mut thinking = Vec::new();
        let mut blocks = Vec::new();
        self.blocks.sort_by_key(|b| b.index);
        for b in self.blocks {
            if b.kind == BlockKind::Thinking {
                let block = ThinkingBlock {
                    text: b.thinking,
                    signature: b.signature,
                };
                thinking.push(block.clone());
                blocks.push(TurnBlock::Thinking(block));
                continue;
            }
            if b.kind == BlockKind::Text {
                if !b.text.is_empty() {
                    blocks.push(TurnBlock::Text(b.text));
                }
                continue;
            }
            if b.name.is_empty() {
                continue;
            }
            if self.refusal
                || self.stop == StopReason::Refusal
                || self.stop == StopReason::MaxTokens
            {
                continue;
            }
            let (input, input_ok) = match serde_json::from_str::<serde_json::Value>(&b.json) {
                Ok(v) => (v, true),
                Err(_) => (serde_json::Value::Null, false),
            };
            // Strict parse: reject trailing garbage the lenient parser
            // would accept... — serde_json already rejects trailing data.
            let call = ToolCallV2 {
                id: b.id,
                name: b.name,
                input,
                input_raw: b.json,
                input_ok,
            };
            blocks.push(TurnBlock::ToolUse(call.clone()));
            calls.push(call);
        }
        TurnOutcome {
            text: self.text,
            thinking,
            calls,
            blocks,
            stop: self.stop,
            usage: self.usage,
        }
    }
}

#[async_trait]
impl LlmClientV2 for Anthropic {
    fn provider_id(&self) -> &'static str {
        "anthropic"
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    async fn stream_turn(
        &self,
        req: ChatReq<'_>,
        tx: tokio::sync::mpsc::UnboundedSender<LlmEvent>,
    ) -> Result<TurnOutcome, ProviderError> {
        let body = self.body(&req);
        let mut last_err = ProviderError::Transport("no attempts".into());
        for attempt in 0..3u32 {
            if req.stop.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            let resp = match self.post(&body, None).await {
                Ok(r) => r,
                Err(ProviderError::Transport(msg)) if msg == "unauthorized" => {
                    // Refresh short-lived ant tokens once, then retry.
                    if attempt == 0 {
                        if let Some(t) = ant_token() {
                            match self.post(&body, Some(t)).await {
                                Ok(r) => r,
                                Err(e) => {
                                    last_err = e;
                                    continue;
                                }
                            }
                        } else {
                            last_err = ProviderError::Transport("unauthorized".into());
                            continue;
                        }
                    } else {
                        last_err = ProviderError::Transport("unauthorized".into());
                        continue;
                    }
                }
                Err(e) => {
                    if is_retryable(&e) && attempt < 2 {
                        backoff_for(&e, attempt).await;
                        last_err = e;
                        continue;
                    }
                    return Err(e);
                }
            };
            match self.read_stream(resp, &req, &tx).await {
                Ok(out) => return Ok(out),
                Err(e) if is_retryable(&e) && attempt < 2 => {
                    backoff_for(&e, attempt).await;
                    last_err = e;
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err)
    }
}

impl Anthropic {
    async fn read_stream(
        &self,
        resp: reqwest::Response,
        req: &ChatReq<'_>,
        tx: &tokio::sync::mpsc::UnboundedSender<LlmEvent>,
    ) -> Result<TurnOutcome, ProviderError> {
        use tokio_stream::StreamExt as _;
        let mut stream = resp.bytes_stream();
        let mut fold = AnthropicFold::default();
        let mut pending: Vec<u8> = Vec::new();
        loop {
            tokio::select! {
                _ = req.stop.cancelled() => {
                    return Err(ProviderError::Cancelled);
                }
                chunk = stream.next() => {
                    let Some(chunk) = chunk else { break };
                    let chunk = chunk.map_err(|e| ProviderError::Transport(e.to_string()))?;
                    pending.extend_from_slice(&chunk);
                    // Byte-buffered line splits: multi-byte characters split
                    // across chunks never corrupt (complete lines decode).
                    while let Some(ix) = pending.iter().position(|&b| b == b'\n') {
                        let line: Vec<u8> = pending.drain(..=ix).collect();
                        let text = String::from_utf8_lossy(&line);
                        let text = text.trim();
                        let Some(data) = text.strip_prefix("data:") else { continue };
                        let data = data.trim();
                        if data.is_empty() {
                            continue;
                        }
                        let v: serde_json::Value = match serde_json::from_str(data) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        fold.feed_event(&v, tx)?;
                    }
                }
            }
        }
        Ok(fold.finish())
    }
}

fn is_retryable(e: &ProviderError) -> bool {
    match e {
        ProviderError::Transport(m) => m.starts_with("retryable:"),
        ProviderError::RateLimited { .. } => true,
        _ => false,
    }
}

async fn backoff(attempt: u32) {
    let base = 500u64 << attempt.min(4);
    tokio::time::sleep(std::time::Duration::from_millis(
        base + rand::random::<u64>() % 250,
    ))
    .await;
}

async fn backoff_for(e: &ProviderError, attempt: u32) {
    match e {
        ProviderError::RateLimited { retry_after_ms } => {
            tokio::time::sleep(std::time::Duration::from_millis(
                (*retry_after_ms).min(30_000),
            ))
            .await;
        }
        _ => backoff(attempt).await,
    }
}

// -- OpenAI-compatible v2 adapter ---------------------------------------------------
// Covers OpenAI, Gemini (generativelanguage OpenAI endpoint), Ollama and any
// base-URL override. No thinking blocks; usage comes from stream_options.

/// Translate v2 messages into OpenAI wire messages. A user message with
/// several tool results becomes one `tool` message per result, because
/// OpenAI pairs exactly one `tool_call_id` with each message.
pub(crate) fn openai_messages(messages: &[Msg]) -> Vec<super::provider::ChatMessage> {
    let mut out = Vec::with_capacity(messages.len() + 1);
    for m in messages {
        match m.role {
            MsgRole::User => {
                let mut text = String::new();
                let mut results = Vec::new();
                for b in &m.blocks {
                    match b {
                        MsgBlock::Text(t) => {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(t);
                        }
                        MsgBlock::Thinking { text: t, .. } => {
                            text.push_str(t);
                        }
                        MsgBlock::ToolUse { .. } => {}
                        MsgBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => {
                            results.push((tool_use_id.clone(), content.clone()));
                        }
                    }
                }
                if !text.is_empty() || results.is_empty() {
                    out.push(super::provider::ChatMessage {
                        role: "user".into(),
                        content: if text.is_empty() { None } else { Some(text) },
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
                for (tool_use_id, content) in results {
                    out.push(super::provider::ChatMessage {
                        role: "tool".into(),
                        content: Some(content),
                        tool_calls: None,
                        tool_call_id: Some(tool_use_id),
                    });
                }
            }
            MsgRole::Assistant => {
                let mut text = String::new();
                let mut tool_calls = Vec::new();
                for b in &m.blocks {
                    match b {
                        MsgBlock::Text(t) => {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(t);
                        }
                        MsgBlock::Thinking { text: t, .. } => {
                            text.push_str(t);
                        }
                        MsgBlock::ToolUse { id, name, input } => {
                            tool_calls.push(super::provider::WireToolCall {
                                id: id.clone(),
                                r#type: "function".into(),
                                function: super::provider::WireFunction {
                                    name: name.clone(),
                                    arguments: input.to_string(),
                                },
                            })
                        }
                        MsgBlock::ToolResult { .. } => {}
                    }
                }
                out.push(super::provider::ChatMessage {
                    role: "assistant".into(),
                    content: if text.is_empty() { None } else { Some(text) },
                    tool_calls: if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    },
                    tool_call_id: None,
                });
            }
        }
    }
    out
}

pub struct CompatV2 {
    kind: ProviderKind,
    inner: super::provider::OpenAiCompat,
}

impl CompatV2 {
    pub fn new(kind: ProviderKind, base_url: String, api_key: String, model: String) -> Self {
        Self {
            kind,
            inner: super::provider::OpenAiCompat::new(base_url, api_key, model),
        }
    }
}

#[async_trait]
impl LlmClientV2 for CompatV2 {
    fn provider_id(&self) -> &'static str {
        match self.kind {
            ProviderKind::OpenAI => "openai",
            ProviderKind::Gemini => "gemini",
            ProviderKind::Ollama => "ollama",
            _ => "openai-compatible",
        }
    }

    fn model_id(&self) -> &str {
        &self.inner.model
    }

    async fn stream_turn(
        &self,
        req: ChatReq<'_>,
        tx: tokio::sync::mpsc::UnboundedSender<LlmEvent>,
    ) -> Result<TurnOutcome, ProviderError> {
        // Translate v2 messages/tools to the OpenAI wire shape (all owned,
        // no static leaks) and fold the SSE stream with usage reporting.
        let mut messages = Vec::with_capacity(req.messages.len() + 1);
        messages.push(super::provider::ChatMessage {
            role: "system".into(),
            content: Some(req.system.to_string()),
            tool_calls: None,
            tool_call_id: None,
        });
        messages.extend(openai_messages(req.messages));
        let tools_json: Vec<serde_json::Value> = req
            .tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    }
                })
            })
            .collect();
        let (text_tx, mut text_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let raw_fut = self.inner.stream_raw(&messages, tools_json, &text_tx);
        tokio::pin!(raw_fut);
        let raw = tokio::select! {
            _ = req.stop.cancelled() => {
                return Err(ProviderError::Cancelled);
            }
            r = &mut raw_fut => r?,
        };
        while let Ok(c) = text_rx.try_recv() {
            let _ = tx.send(LlmEvent::Text(c));
        }
        let stop = if raw.refusal {
            StopReason::Refusal
        } else if !raw.tool_calls.is_empty() {
            StopReason::ToolUse
        } else if raw.finish_reason.as_deref() == Some("length") {
            StopReason::MaxTokens
        } else {
            StopReason::EndTurn
        };
        // Never run tools from truncated or refused turns.
        let calls = if stop == StopReason::EndTurn || stop == StopReason::ToolUse {
            raw.tool_calls
                .into_iter()
                .map(|c| {
                    let (input, input_ok) =
                        match serde_json::from_str::<serde_json::Value>(&c.function.arguments) {
                            Ok(v) => (v, true),
                            Err(_) => (serde_json::Value::Null, false),
                        };
                    ToolCallV2 {
                        id: c.id,
                        name: c.function.name,
                        input,
                        input_raw: c.function.arguments,
                        input_ok,
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let stop = if calls.is_empty() && stop == StopReason::ToolUse {
            StopReason::EndTurn
        } else {
            stop
        };
        Ok(TurnOutcome {
            text: raw.content.clone().unwrap_or_default(),
            thinking: Vec::new(),
            calls: calls.clone(),
            blocks: std::iter::once(raw.content)
                .flatten()
                .filter(|text| !text.is_empty())
                .map(TurnBlock::Text)
                .chain(calls.into_iter().map(TurnBlock::ToolUse))
                .collect(),
            stop,
            usage: Usage {
                input: raw.prompt_tokens,
                output: raw.completion_tokens,
                ..Default::default()
            },
        })
    }
}

/// Build the v2 client for a resolved spec.
pub fn make_client(spec: &ProviderSpec) -> Result<ArcV2, ProviderError> {
    match spec.kind {
        ProviderKind::Anthropic => {
            Ok(Arc::new(Anthropic::new(spec.model.clone(), spec.base_url.clone())?) as ArcV2)
        }
        ProviderKind::OpenAI
        | ProviderKind::Gemini
        | ProviderKind::Ollama
        | ProviderKind::Compat => {
            let key = match spec.kind {
                ProviderKind::OpenAI => std::env::var("OPENAI_API_KEY").unwrap_or_default(),
                ProviderKind::Gemini => std::env::var("GEMINI_API_KEY").unwrap_or_default(),
                _ => "ollama".into(),
            };
            if key.trim().is_empty()
                && spec.kind != ProviderKind::Ollama
                && spec.kind != ProviderKind::Compat
            {
                return Err(ProviderError::NoKey);
            }
            Ok(Arc::new(CompatV2::new(
                spec.kind,
                spec.base_url.clone(),
                key,
                spec.model.clone(),
            )) as ArcV2)
        }
    }
}

pub type ArcV2 = std::sync::Arc<dyn LlmClientV2>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_tool_results_become_separate_messages() {
        let messages = vec![Msg {
            role: MsgRole::User,
            blocks: vec![
                MsgBlock::Text("results".into()),
                MsgBlock::ToolResult {
                    tool_use_id: "a".into(),
                    content: "one".into(),
                    is_error: false,
                },
                MsgBlock::ToolResult {
                    tool_use_id: "b".into(),
                    content: "two".into(),
                    is_error: true,
                },
            ],
            cache: false,
        }];
        let wire = openai_messages(&messages);
        assert_eq!(wire.len(), 3);
        assert_eq!(wire[0].role, "user");
        assert_eq!(wire[0].content.as_deref(), Some("results"));
        assert_eq!(wire[1].role, "tool");
        assert_eq!(wire[1].tool_call_id.as_deref(), Some("a"));
        assert_eq!(wire[1].content.as_deref(), Some("one"));
        assert_eq!(wire[2].tool_call_id.as_deref(), Some("b"));
        assert_eq!(wire[2].content.as_deref(), Some("two"));
    }
}
