#!/usr/bin/env bash
# Optional host smoke against a running serpotter instance.
# Not wired into CI — personal-use / cutover only.
#
# Usage:
#   export SERPOTTER_TOKEN=tok-...
#   # optional: BASE_URL=http://127.0.0.1:8080
#   ./scripts/live-smoke.sh
set -euo pipefail

BASE_URL="${BASE_URL:-http://127.0.0.1:8080}"
BASE_URL="${BASE_URL%/}"

usage() {
  cat <<'EOF' >&2
Usage: SERPOTTER_TOKEN=tok-... [BASE_URL=http://127.0.0.1:8080] ./scripts/live-smoke.sh

Optional host smoke (not CI). Requires a live serpotter process and a client tok-.
Steps: GET /live, GET /ready, POST search/extract/research, MCP initialize + tools/list.
EOF
}

if [[ -z "${SERPOTTER_TOKEN:-}" ]]; then
  usage
  exit 2
fi

AUTH_HDR=( -H "Authorization: Bearer ${SERPOTTER_TOKEN}" )
JSON_HDR=( -H "content-type: application/json" )
MCP_ACCEPT=( -H "accept: application/json, text/event-stream" )

step() {
  printf 'ok  %s\n' "$1"
}

fail() {
  printf 'fail %s\n' "$1" >&2
  exit 1
}

# curl -f fails on HTTP non-2xx; -s silent body, -S show errors
req() {
  local label="$1"
  shift
  if ! curl -fsS "$@" >/dev/null; then
    fail "$label"
  fi
  step "$label"
}

req "GET /live" "${BASE_URL}/live"
req "GET /ready" "${BASE_URL}/ready"

req "POST /api/search" \
  -X POST "${BASE_URL}/api/search" \
  "${AUTH_HDR[@]}" \
  "${JSON_HDR[@]}" \
  -d '{"query":"smoke","maxResults":3}'

req "POST /api/extract" \
  -X POST "${BASE_URL}/api/extract" \
  "${AUTH_HDR[@]}" \
  "${JSON_HDR[@]}" \
  -d '{"url":"https://example.com"}'

req "POST /api/research" \
  -X POST "${BASE_URL}/api/research" \
  "${AUTH_HDR[@]}" \
  "${JSON_HDR[@]}" \
  -d '{"query":"smoke","maxResults":2,"extractTopN":1}'

HDR_FILE="$(mktemp)"
trap 'rm -f "$HDR_FILE"' EXIT

# MCP initialize mints Mcp-Session-Id; body may be bare JSON or SSE.
if ! curl -fsS -D "$HDR_FILE" -o /dev/null \
  -X POST "${BASE_URL}/mcp" \
  "${AUTH_HDR[@]}" \
  "${JSON_HDR[@]}" \
  "${MCP_ACCEPT[@]}" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"live-smoke","version":"0.1.0"}}}'; then
  fail "POST /mcp initialize"
fi
step "POST /mcp initialize"

SID="$(grep -i '^mcp-session-id:' "$HDR_FILE" | awk '{print $2}' | tr -d '\r' || true)"
if [[ -z "$SID" ]]; then
  fail "POST /mcp initialize (missing Mcp-Session-Id)"
fi

req "POST /mcp tools/list" \
  -X POST "${BASE_URL}/mcp" \
  "${AUTH_HDR[@]}" \
  "${JSON_HDR[@]}" \
  "${MCP_ACCEPT[@]}" \
  -H "mcp-session-id: ${SID}" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'

printf 'live-smoke passed against %s\n' "$BASE_URL"
