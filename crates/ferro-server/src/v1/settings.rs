//! Settings + session endpoints (API.md §§ 3.2–3.3).

use axum::{
    body::Bytes,
    extract::{Query, State},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/settings", get(settings_get).put(settings_put))
        .route("/api/v1/settings/schema", get(settings_schema))
}

#[derive(Deserialize, Default)]
struct ScopeParam {
    scope: Option<String>,
}

async fn settings_get(
    State(s): State<Arc<AppState>>,
    Query(p): Query<ScopeParam>,
) -> Json<serde_json::Value> {
    let scope = p.scope.unwrap_or_else(|| "effective".into());
    let values = match scope.as_str() {
        "user" | "workspace" => {
            serde_json::Value::Object(s.settings.raw(&scope).into_iter().collect())
        }
        _ => serde_json::Value::Object(s.settings.effective().into_iter().collect()),
    };
    Json(serde_json::json!({
        "scope": scope,
        "values": values,
        "defaults": defaults_map(),
    }))
}

fn defaults_map() -> std::collections::BTreeMap<String, serde_json::Value> {
    crate::state::SettingsStore::schema()
        .into_iter()
        .filter_map(|d| {
            let k = d.get("key")?.as_str()?.to_string();
            Some((k, d.get("default")?.clone()))
        })
        .collect()
}

async fn settings_put(
    State(s): State<Arc<AppState>>,
    Query(p): Query<ScopeParam>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    if body.len() > 1024 * 1024 {
        return Err(ApiError::new(
            crate::error::ErrorCode::TooLarge,
            "body over 1 MiB",
        ));
    }
    let scope = p.scope.unwrap_or_else(|| "workspace".into());
    if scope != "user" && scope != "workspace" {
        return Err(ApiError::bad_request("scope must be user|workspace"));
    }
    let v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))?;
    let values = v.get("values").cloned().unwrap_or(serde_json::Value::Null);
    let map: std::collections::BTreeMap<String, serde_json::Value> = match values {
        serde_json::Value::Object(m) => m.into_iter().collect(),
        _ => return Err(ApiError::bad_request("body.values must be an object")),
    };
    let eff = s
        .settings
        .save(&scope, map)
        .map_err(ApiError::bad_request)?;
    s.bus.publish(crate::bus::ServerEvent::Settings {
        values: serde_json::json!(eff),
    });
    Ok(Json(
        serde_json::json!({ "scope": "effective", "values": eff, "defaults": defaults_map() }),
    ))
}

async fn settings_schema() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "keys": crate::state::SettingsStore::schema() }))
}
