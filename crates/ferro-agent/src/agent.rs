//! Agent loop: system prompt -> chat -> dispatch tools -> repeat.
//! Read-only by default. Stops at final text or max_steps.

use std::sync::Arc;

use serde::Serialize;

use crate::provider::{ChatMessage, LlmClient};
use crate::tools::{self, ToolCall, ToolResult};
use crate::{Access, Sandbox};
use ferro_core::Index;

#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub thought: Option<String>,
    pub calls: Vec<(ToolCall, ToolResult)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Transcript {
    pub steps: Vec<Step>,
    pub final_text: String,
    pub truncated: bool,
}

pub struct Agent<C: LlmClient> {
    pub index: Arc<Index>,
    pub sandbox: Sandbox,
    pub client: Arc<C>,
    pub max_steps: usize,
}

impl<C: LlmClient> Agent<C> {
    pub fn system_prompt(&self) -> String {
        let mut t = String::from(
            "You are Ferro, a code assistant inside a fast local code-review tool. \
             Answer questions about the workspace using the provided tools. \
             Prefer read_file windows over full reads. Cite file:line for claims. \
             Be concise. Write tools are disabled unless the user enabled them; \
             if asked to edit, explain what you would change and stop.\n\nTools:\n",
        );
        for d in tools::registry() {
            let access = match d.access {
                crate::AccessLabel::Read => "read",
                crate::AccessLabel::Write => "write",
                crate::AccessLabel::Destructive => "destructive",
            };
            t.push_str(&format!("- {} [{access}]: {}\n", d.name, d.description));
        }
        t
    }

    pub async fn run(&self, prompt: &str) -> Transcript {
        let defs = tools::registry();
        let mut messages = vec![
            ChatMessage {
                role: "system".into(),
                content: Some(self.system_prompt()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".into(),
                content: Some(prompt.to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
        ];
        let mut steps = Vec::new();
        for _ in 0..self.max_steps.max(1) {
            let turn = match self.client.chat(&messages, &defs).await {
                Ok(t) => t,
                Err(e) => {
                    return Transcript {
                        steps,
                        final_text: format!("provider error: {e}"),
                        truncated: true,
                    };
                }
            };
            if turn.tool_calls.is_empty() {
                return Transcript {
                    steps,
                    final_text: turn.content.unwrap_or_default(),
                    truncated: false,
                };
            }
            // Echo assistant tool calls back into history (OpenAI format).
            messages.push(ChatMessage {
                role: "assistant".into(),
                content: turn.content.clone(),
                tool_calls: Some(turn.tool_calls.clone()),
                tool_call_id: None,
            });
            let mut step = Step {
                thought: turn.content,
                calls: Vec::new(),
            };
            for wc in &turn.tool_calls {
                let args: serde_json::Value =
                    serde_json::from_str(&wc.function.arguments).unwrap_or(serde_json::json!({}));
                let call = ToolCall {
                    name: wc.function.name.clone(),
                    args,
                };
                // Sandbox gate lives in dispatch via check; resolve write paths here.
                let _ = self.sandbox.check(Access::Read);
                let result = tools::dispatch(&self.index, &self.sandbox, &call);
                messages.push(ChatMessage {
                    role: "tool".into(),
                    content: Some(result.output.clone()),
                    tool_calls: None,
                    tool_call_id: Some(wc.id.clone()),
                });
                step.calls.push((call, result));
            }
            steps.push(step);
        }
        Transcript {
            steps,
            final_text: "(max steps reached — answer may be incomplete)".into(),
            truncated: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ProviderError, Turn};
    use async_trait::async_trait;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Mock {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl LlmClient for Mock {
        async fn chat(
            &self,
            _messages: &[ChatMessage],
            _tools: &[crate::ToolDef],
        ) -> Result<Turn, ProviderError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(Turn {
                    content: Some("looking".into()),
                    tool_calls: vec![crate::provider::WireToolCall {
                        id: "c1".into(),
                        r#type: "function".into(),
                        function: crate::provider::WireFunction {
                            name: "git_status".into(),
                            arguments: "{}".into(),
                        },
                    }],
                })
            } else {
                Ok(Turn {
                    content: Some("done: tree is clean".into()),
                    tool_calls: vec![],
                })
            }
        }
    }

    #[tokio::test]
    async fn loop_runs_tool_then_answers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::File::create(dir.path().join("a.txt"))
            .unwrap()
            .write_all(b"hi")
            .unwrap();
        let idx = Arc::new(Index::new(dir.path().to_path_buf()));
        let sb = Sandbox::readonly(idx.root().to_path_buf());
        let agent = Agent {
            index: idx,
            sandbox: sb,
            client: Arc::new(Mock {
                calls: AtomicUsize::new(0),
            }),
            max_steps: 4,
        };
        let t = agent.run("is the tree clean?").await;
        assert_eq!(t.steps.len(), 1);
        assert_eq!(t.steps[0].calls.len(), 1);
        assert!(t.final_text.contains("clean"));
        assert!(!t.truncated);
    }

    #[tokio::test]
    async fn loop_stops_at_max_steps() {
        struct AlwaysTool;
        #[async_trait]
        impl LlmClient for AlwaysTool {
            async fn chat(
                &self,
                _m: &[ChatMessage],
                _t: &[crate::ToolDef],
            ) -> Result<Turn, ProviderError> {
                Ok(Turn {
                    content: None,
                    tool_calls: vec![crate::provider::WireToolCall {
                        id: "c".into(),
                        r#type: "function".into(),
                        function: crate::provider::WireFunction {
                            name: "git_status".into(),
                            arguments: "{}".into(),
                        },
                    }],
                })
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let idx = Arc::new(Index::new(dir.path().to_path_buf()));
        let sb = Sandbox::readonly(idx.root().to_path_buf());
        let agent = Agent {
            index: idx,
            sandbox: sb,
            client: Arc::new(AlwaysTool),
            max_steps: 2,
        };
        let t = agent.run("x").await;
        assert_eq!(t.steps.len(), 2);
        assert!(t.truncated);
    }
}
