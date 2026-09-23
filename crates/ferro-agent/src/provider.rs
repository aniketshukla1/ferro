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

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[crate::ToolDef],
    ) -> Result<Turn, ProviderError>;
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
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            model: model.into(),
            client: reqwest::Client::new(),
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
            tool_choice: "auto",
        };
        let resp = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&req)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
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
    tools: Vec<serde_json::Value>,
    tool_choice: &'static str,
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
            tool_choice: "auto",
        };
        let resp = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
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
        Ok(Turn {
            content: msg.content,
            tool_calls: msg.tool_calls.unwrap_or_default(),
        })
    }
}
