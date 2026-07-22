//! Lean JSON-RPC MCP over POST /mcp.
//! Tool args accept mysearch snake_case (preferred) and camelCase aliases.

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
    #[allow(dead_code)]
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
}

pub async fn mcp_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    // Auth for all MCP methods except optional initialize without tools
    let req: JsonRpcRequest = match serde_json::from_value(body) {
        Ok(r) => r,
        Err(e) => {
            return rpc_err(None, -32700, format!("parse error: {e}")).into_response();
        }
    };

    // initialize may run before auth in some clients; still require token for tool use
    let needs_auth = req.method != "initialize" && req.method != "ping";
    if needs_auth {
        if let Err(r) = require_api_token(&state, &headers).await {
            // MCP middleware shape in mysearch is {detail}; keep problem for REST consistency on HTTP layer
            // but tool path uses JSON-RPC envelope after auth. Missing token → 401 problem+json.
            return r;
        }
    } else if req.method == "initialize" {
        // optional: still accept unauthenticated initialize
    }

    match req.method.as_str() {
        "initialize" => rpc_ok(
            req.id,
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "serpotter", "version": "0.1.0" }
            }),
        )
        .into_response(),
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
                                "max_results": { "type": "integer", "minimum": 1, "maximum": 20 },
                                "include_content": { "type": "boolean" },
                                "mode": { "type": "string" },
                                "provider": { "type": "string" }
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
                        "description": "Search then scrape top results (mysearch research tool)",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "query": { "type": "string" },
                                "web_max_results": { "type": "integer", "minimum": 1, "maximum": 20 },
                                "social_max_results": { "type": "integer", "minimum": 0, "maximum": 10 },
                                "scrape_top_n": { "type": "integer", "minimum": 0, "maximum": 10 },
                                "mode": { "type": "string" },
                                "strategy": { "type": "string" }
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
            if let Err(r) = require_api_token(&state, &headers).await {
                return r;
            }
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

/// Prefer snake_case key, then camelCase alias (mysearch MCP uses snake_case).
fn arg_u32(args: &Value, snake: &str, camel: &str) -> Option<u32> {
    args.get(snake)
        .or_else(|| args.get(camel))
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
}

fn arg_bool(args: &Value, snake: &str, camel: &str) -> Option<bool> {
    args.get(snake)
        .or_else(|| args.get(camel))
        .and_then(|v| v.as_bool())
}

fn arg_str<'a>(args: &'a Value, snake: &str, camel: &str) -> Option<&'a str> {
    args.get(snake)
        .or_else(|| args.get(camel))
        .and_then(|v| v.as_str())
}

async fn call_tool(state: &AppState, name: &str, args: Value) -> Result<String, String> {
    match name {
        "search" => {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing query".to_string())?;
            let max_results = arg_u32(&args, "max_results", "maxResults");
            let include_content = arg_bool(&args, "include_content", "includeContent");
            let mode = arg_str(&args, "mode", "mode").map(str::to_string);
            let provider = arg_str(&args, "provider", "provider").map(str::to_string);
            let body = SearchQuery {
                query: query.to_string(),
                max_results,
                include_content,
                mode,
                provider,
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
            let provider = arg_str(&args, "provider", "provider");
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
            // mysearch MCP: web_max_results / social_max_results / scrape_top_n
            // also accept max_results / extractTopN aliases
            let web_max = arg_u32(&args, "web_max_results", "webMaxResults")
                .or_else(|| arg_u32(&args, "max_results", "maxResults"));
            let scrape_top = arg_u32(&args, "scrape_top_n", "scrapeTopN")
                .or_else(|| arg_u32(&args, "extract_top_n", "extractTopN"));
            let social_max = arg_u32(&args, "social_max_results", "socialMaxResults");
            let body = ResearchRequest {
                query: query.to_string(),
                web_max_results: web_max,
                scrape_top_n: scrape_top,
                include_content: None,
                social_max_results: social_max,
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
        }),
    })
}

#[allow(dead_code)]
fn _status_unused() -> StatusCode {
    StatusCode::OK
}
