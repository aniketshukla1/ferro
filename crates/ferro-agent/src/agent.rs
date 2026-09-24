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
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    Thought {
        text: String,
    },
    ToolStart {
        name: String,
        args: serde_json::Value,
    },
    ToolResult {
        name: String,
        ok: bool,
        output: String,
        truncated: bool,
    },
    Token {
        text: String,
    },
    Final {
        text: String,
        truncated: bool,
    },
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
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let t = self.run_stream(prompt, tx).await;
        // Drain any leftover events (all consumed into the transcript already).
        while rx.try_recv().is_ok() {}
        t
    }

    /// Drive one provider turn, forwarding token deltas live. Borrow ends on return.
    /// Returns leftover deltas that landed with turn completion (emitted first).
    async fn drive_turn(
        &self,
        messages: &[ChatMessage],
        defs: &[crate::ToolDef],
        send: &impl Fn(AgentEvent),
    ) -> (
        Result<crate::provider::Turn, crate::provider::ProviderError>,
        Vec<String>,
    ) {
        let (dtick, mut dtick_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut fut = Box::pin(self.client.chat_stream(messages, defs, dtick));
        let r = loop {
            tokio::select! {
                r = &mut fut => break r,
                d = dtick_rx.recv() => {
                    if let Some(x) = d {
                        if let Some(c) = x.content_piece {
                            send(AgentEvent::Token { text: c });
                        }
                    }
                }
            }
        };
        let mut leftover = Vec::new();
        while let Ok(d) = dtick_rx.try_recv() {
            if let Some(c) = d.content_piece {
                leftover.push(c);
            }
        }
        (r, leftover)
    }

    /// Same loop as `run`, but emits live events as each step unfolds.
    pub async fn run_stream(
        &self,
        prompt: &str,
        tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    ) -> Transcript {
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
        let send = |e: AgentEvent| {
            let _ = tx.send(e);
        };
        for _ in 0..self.max_steps.max(1) {
            let (r, leftover) = self.drive_turn(&messages, &defs, &send).await;
            for tok in leftover {
                send(AgentEvent::Token { text: tok });
            }
            let turn = match r {
                Ok(t) => t,
                Err(e) => {
                    let final_text = format!("provider error: {e}");
                    send(AgentEvent::Final {
                        text: final_text.clone(),
                        truncated: true,
                    });
                    return Transcript {
                        steps,
                        final_text,
                        truncated: true,
                    };
                }
            };
            if turn.tool_calls.is_empty() {
                let final_text = turn.content.unwrap_or_default();
                send(AgentEvent::Final {
                    text: final_text.clone(),
                    truncated: false,
                });
                return Transcript {
                    steps,
                    final_text,
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
            if let Some(thought) = turn.content.clone() {
                send(AgentEvent::Thought {
                    text: thought.clone(),
                });
            }
            let mut step = Step {
                thought: turn.content,
                calls: Vec::new(),
            };
            for wc in &turn.tool_calls {
                let args: serde_json::Value =
                    serde_json::from_str(&wc.function.arguments).unwrap_or(serde_json::json!({}));
                let call = ToolCall {
                    name: wc.function.name.clone(),
                    args: args.clone(),
                };
                send(AgentEvent::ToolStart {
                    name: call.name.clone(),
                    args,
                });
                // Sandbox gate lives in dispatch via check; resolve write paths here.
                let _ = self.sandbox.check(Access::Read);
                let result = tools::dispatch(&self.index, &self.sandbox, &call);
                send(AgentEvent::ToolResult {
                    name: call.name.clone(),
                    ok: result.ok,
                    output: result.output.chars().take(2000).collect(),
                    truncated: result.truncated,
                });
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
        let final_text = "(max steps reached — answer may be incomplete)".to_string();
        send(AgentEvent::Final {
            text: final_text.clone(),
            truncated: true,
        });
        Transcript {
            steps,
            final_text,
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

    #[tokio::test]
    async fn loop_applies_patch_end_to_end() {
        // Temp git repo: agent's apply_patch must change the file.
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(r)
                .args(&args)
                .status()
                .unwrap()
                .success());
        }
        std::fs::File::create(r.join("a.txt"))
            .unwrap()
            .write_all(b"one\n")
            .unwrap();
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(r)
            .args(["add", "."])
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(r)
            .args(["commit", "-m", "init"])
            .status()
            .unwrap()
            .success());

        struct Fixer;
        #[async_trait]
        impl LlmClient for Fixer {
            async fn chat(
                &self,
                _m: &[ChatMessage],
                _t: &[crate::ToolDef],
            ) -> Result<Turn, ProviderError> {
                static N: AtomicUsize = AtomicUsize::new(0);
                if N.fetch_add(1, Ordering::SeqCst) == 0 {
                    Ok(Turn {
                        content: Some("fixing".into()),
                        tool_calls: vec![crate::provider::WireToolCall {
                            id: "c1".into(),
                            r#type: "function".into(),
                            function: crate::provider::WireFunction {
                                name: "apply_patch".into(),
                                arguments: "{\"patch\":\"diff --git a/a.txt b/a.txt\\n--- a/a.txt\\n+++ b/a.txt\\n@@ -1 +1,2 @@\\n one\\n+two\\n\"}".into(),
                            },
                        }],
                    })
                } else {
                    Ok(Turn {
                        content: Some("fixed".into()),
                        tool_calls: vec![],
                    })
                }
            }
        }
        let idx = Arc::new(Index::new(r.to_path_buf()));
        let mut sb = Sandbox::readonly(idx.root().to_path_buf());
        sb.allow_write = true;
        let agent = Agent {
            index: idx,
            sandbox: sb,
            client: Arc::new(Fixer),
            max_steps: 4,
        };
        let t = agent.run("address the comment").await;
        assert_eq!(
            std::fs::read_to_string(r.join("a.txt")).unwrap(),
            "one\ntwo\n"
        );
        assert!(t.final_text.contains("fixed"));
    }

    #[tokio::test]
    async fn stream_emits_ordered_events() {
        let dir = tempfile::tempdir().unwrap();
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
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let t = agent.run_stream("is the tree clean?", tx).await;
        drop(agent);
        let mut kinds = vec![];
        while let Some(ev) = rx.recv().await {
            kinds.push(match ev {
                AgentEvent::Thought { .. } => "thought",
                AgentEvent::ToolStart { .. } => "start",
                AgentEvent::ToolResult { .. } => "result",
                AgentEvent::Token { .. } => "token",
                AgentEvent::Final { .. } => "final",
            });
        }
        assert_eq!(
            kinds,
            vec!["token", "thought", "start", "result", "token", "final"]
        );
        assert!(t.final_text.contains("clean"));
    }
}
