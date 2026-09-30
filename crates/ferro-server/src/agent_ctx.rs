//! Shared agent tool context for AI and MCP (B5 policy from effective settings).

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::state::{AppState, Workspace};

fn redact_enabled(eff: &BTreeMap<String, serde_json::Value>) -> bool {
    eff.get("ai.redactSecrets")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

fn never_send_globs(eff: &BTreeMap<String, serde_json::Value>) -> Vec<String> {
    eff.get("ai.neverSend")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|x| x.to_string()))
                .collect()
        })
        .unwrap_or_else(ferro_agent::default_never_send)
}

/// ToolCtx with redaction, never-send, search excludes, and symbol index.
pub fn tool_ctx_for(ws: &Arc<Workspace>, s: &AppState) -> Arc<ferro_agent::ToolCtx> {
    let eff = s.settings.effective(&ws.key);
    let mut ctx = ferro_agent::ToolCtx::new(ws.index.clone());
    ctx.set_policy(redact_enabled(&eff), &never_send_globs(&eff));
    ctx.symbols = Some(ws.symbols.clone());
    if let Some(ex) = eff.get("search.exclude").and_then(|v| v.as_array()) {
        ctx.default_exclude = ex
            .iter()
            .filter_map(|x| x.as_str().map(|x| x.to_string()))
            .collect();
    }
    Arc::new(ctx)
}
