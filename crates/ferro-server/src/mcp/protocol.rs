//! Minimal JSON-RPC handler for MCP (stdio + HTTP). No third-party MCP SDK.

use std::sync::Arc;

use serde_json::Value;

use crate::state::AppState;

use super::tools::{self, PROTOCOL_VERSION};

pub async fn handle_message(s: &Arc<AppState>, msg: Value, version: &str) -> Option<Value> {
    if !msg.is_object() {
        return Some(rpc_error(Value::Null, -32600, "invalid request"));
    }
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    // Notifications have no id — no response.
    if id.is_none() && method.starts_with("notifications/") {
        return None;
    }
    if method.is_empty() {
        return Some(rpc_error(
            id.unwrap_or(Value::Null),
            -32600,
            "invalid request",
        ));
    }
    let id = id.unwrap_or(Value::Null);

    let result = match method {
        "initialize" => Ok(initialize(version)),
        "ping" => Ok(serde_json::json!({})),
        "tools/list" => Ok(serde_json::json!({ "tools": tools::tool_definitions() })),
        "tools/call" => {
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or(Value::Object(serde_json::Map::new()));
            if name.is_empty() {
                Err((-32602, "missing tool name"))
            } else if !serde_json::from_value::<serde_json::Map<String, Value>>(args.clone())
                .is_ok()
            {
                Err((-32602, "arguments must be an object"))
            } else {
                Ok(tools::call_async(s, name, args).await)
            }
        }
        _ => Err((-32601, "method not found")),
    };

    match result {
        Ok(v) => Some(rpc_ok(id, v)),
        Err((code, msg)) => Some(rpc_error(id, code, msg)),
    }
}

fn initialize(version: &str) -> Value {
    serde_json::json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "ferro", "version": version },
    })
}

fn rpc_ok(id: Value, result: Value) -> Value {
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i32, message: impl Into<String>) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message.into() }
    })
}
