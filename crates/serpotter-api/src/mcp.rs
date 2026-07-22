//! Lean JSON-RPC MCP over POST /mcp.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use serpotter_core::SearchQuery;
use serpotter_db::EXPECTED_SCHEMA_VERSION;

use crate::extract::{extract_url, research_inner, ResearchRequest};
use crate::search::search_inner;
use crate::{require_api_token, AppState};

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: Option<String>,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

pub async fn mcp_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    // notifications may omit id; still require auth for lean parity with REST tools
    if let Err(r) = require_api_token(&state, &headers).await {
        return r;
    }

    // Support single object or batch array (lean: single only)
    let req: JsonRpcRequest = match serde_json::from_value(body) {
        Ok(r) => r,
        Err(e) => {
            return Json(JsonRpcResponse {
                jsonrpc: "2.0",
                id: None,
                result: None,
                error: Some(JsonRpcError {
                    code: -32700,
                    message: format!("parse error: {e}"),
                    data: None,
                }),
            })
            .into_response();
        }
    };

    if req.jsonrpc.as_deref().unwrap_or("2.0") != "2.0" {
        return rpc_err(req.id, -32600, "invalid jsonrpc version").into_response();
    }

    match req.method.as_str() {
        "initialize" => {
            let result = json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "serpotter", "version": "0.1.0" }
            });
            rpc_ok(req.id, result).into_response()
        }
        "notifications/initialized" | "initialized" => {
            // notification — empty 202-style OK body
            StatusCode::NO_CONTENT.into_response()
        }
        "tools/list" => {
            let tools = json!({
                "tools": [
                    {
                        "name": "search",
                        "description": "Web search via multi-provider routing",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "query": { "type": "string" },
                                "maxResults": { "type": "integer" },
                                "includeContent": { "type": "boolean" }
                            },
                            "required": ["query"]
                        }
                    },
                    {
                        "name": "extract_url",
                        "description": "Scrape/extract a URL (Firecrawl then Tavily)",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "url": { "type": "string" },
                                "provider": { "type": "string" }
                            },
                            "required": ["url"]
                        }
                    },
                    {
                        "name": "research",
                        "description": "Search then extract top N results",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "query": { "type": "string" },
                                "maxResults": { "type": "integer" },
                                "extractTopN": { "type": "integer" }
                            },
                            "required": ["query"]
                        }
                    },
                    {
                        "name": "mysearch_health",
                        "description": "Health and schema version",
                        "inputSchema": { "type": "object", "properties": {} }
                    }
                ]
            });
            rpc_ok(req.id, tools).into_response()
        }
        "tools/call" => {
            let name = req
                .params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let args = req
                .params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match call_tool(&state, name, args).await {
                Ok(content) => {
                    let result = json!({
                        "content": [{ "type": "text", "text": content }],
                        "isError": false
                    });
                    rpc_ok(req.id, result).into_response()
                }
                Err(msg) => {
                    let result = json!({
                        "content": [{ "type": "text", "text": msg }],
                        "isError": true
                    });
                    rpc_ok(req.id, result).into_response()
                }
            }
        }
        "ping" => rpc_ok(req.id, json!({})).into_response(),
        other => rpc_err(req.id, -32601, format!("method not found: {other}")).into_response(),
    }
}

async fn call_tool(state: &AppState, name: &str, args: Value) -> Result<String, String> {
    match name {
        "search" => {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing query".to_string())?;
            let max_results = args
                .get("maxResults")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32);
            let include_content = args.get("includeContent").and_then(|v| v.as_bool());
            let body = SearchQuery {
                query: query.to_string(),
                max_results,
                include_content,
                ..Default::default()
            };
            let resp = search_inner(state, body)
                .await
                .map_err(|e| format!("search failed: {e:?}"))?;
            serde_json::to_string_pretty(&resp).map_err(|e| e.to_string())
        }
        "extract_url" => {
            let url = args
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing url".to_string())?;
            let provider = args.get("provider").and_then(|v| v.as_str());
            let resp = extract_url(state, url, provider)
                .await
                .map_err(|e| format!("extract failed: {e:?}"))?;
            serde_json::to_string_pretty(&resp).map_err(|e| e.to_string())
        }
        "research" => {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing query".to_string())?;
            let body = ResearchRequest {
                query: query.to_string(),
                max_results: args
                    .get("maxResults")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32),
                extract_top_n: args
                    .get("extractTopN")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32),
                include_content: None,
            };
            let resp = research_inner(state, body)
                .await
                .map_err(|e| format!("research failed: {e:?}"))?;
            serde_json::to_string_pretty(&resp).map_err(|e| e.to_string())
        }
        "mysearch_health" => {
            let version = state.db.schema_version().await.ok();
            let body = json!({
                "status": "ok",
                "schemaVersion": version,
                "expected": EXPECTED_SCHEMA_VERSION,
            });
            Ok(body.to_string())
        }
        other => Err(format!("unknown tool: {other}")),
    }
}

fn rpc_ok(id: Option<Value>, result: Value) -> Json<JsonRpcResponse> {
    Json(JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: Some(result),
        error: None,
    })
}

fn rpc_err(id: Option<Value>, code: i64, message: impl Into<String>) -> Json<JsonRpcResponse> {
    Json(JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.into(),
            data: None,
        }),
    })
}
