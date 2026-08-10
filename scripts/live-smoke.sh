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
Steps: GET /live, GET /ready, POST search/extract/research, MCP server/discover + tools/list.
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

# MCP 2026-07-28 stateless: server/discover then tools/list (no session).
# Body may be bare JSON or SSE.
req "POST /mcp server/discover" \
  -X POST "${BASE_URL}/mcp" \
  "${AUTH_HDR[@]}" \
  "${JSON_HDR[@]}" \
  "${MCP_ACCEPT[@]}" \
  -H "MCP-Protocol-Version: 2026-07-28" \
  -H "Mcp-Method: server/discover" \
  -d '{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}'

req "POST /mcp tools/list" \
  -X POST "${BASE_URL}/mcp" \
  "${AUTH_HDR[@]}" \
  "${JSON_HDR[@]}" \
  "${MCP_ACCEPT[@]}" \
  -H "MCP-Protocol-Version: 2026-07-28" \
  -H "Mcp-Method: tools/list" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}'

printf 'live-smoke passed against %s\n' "$BASE_URL"
