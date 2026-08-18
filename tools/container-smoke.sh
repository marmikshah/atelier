#!/usr/bin/env bash
set -euo pipefail

image=${1:?usage: tools/container-smoke.sh <image>}
container="atelier-smoke-$$"
volume="atelier-smoke-data-$$"
responses=$(mktemp -d)
token="atelier-container-smoke-${GITHUB_RUN_ID:-local}-$$"
endpoint=

cleanup() {
  docker rm -f "$container" >/dev/null 2>&1 || true
  docker volume rm -f "$volume" >/dev/null 2>&1 || true
  rm -rf "$responses"
}
trap cleanup EXIT

docker volume create "$volume" >/dev/null

start_server() {
  docker run -d --name "$container" \
    --read-only \
    --tmpfs /tmp:rw,nosuid,nodev,noexec,size=16m,mode=1777 \
    --security-opt no-new-privileges:true \
    --cap-drop ALL \
    --mount "type=volume,src=$volume,dst=/data" \
    -p 127.0.0.1::8765 \
    -e ATELIER_HTTP_TOKEN="$token" \
    "$image" >/dev/null

  local address
  address=$(docker port "$container" 8765/tcp)
  endpoint="http://$address/mcp"
}

initialize() {
  local output=$1
  local ready=false
  local attempt=0
  while [ "$attempt" -lt 30 ]; do
    attempt=$((attempt + 1))
    if curl --fail --silent --show-error \
      -X POST "$endpoint" \
      -H 'content-type: application/json' \
      -H 'accept: application/json, text/event-stream' \
      -H "authorization: Bearer $token" \
      --data '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"container-smoke","version":"0"}}}' \
      >"$output"; then
      ready=true
      break
    fi
    sleep 1
  done
  if [ "$ready" != true ]; then
    docker logs "$container"
    echo "container smoke: server did not answer initialize" >&2
    return 1
  fi
  jq -e '.result.serverInfo.name == "atelier"' "$output" >/dev/null

  local initialized_status
  initialized_status=$(curl --silent --output /dev/null --write-out '%{http_code}' \
    -X POST "$endpoint" \
    -H 'content-type: application/json' \
    -H 'accept: application/json, text/event-stream' \
    -H "authorization: Bearer $token" \
    --data '{"jsonrpc":"2.0","method":"notifications/initialized"}')
  test "$initialized_status" = 202
}

mcp_call() {
  local payload=$1
  local output=$2
  curl --fail --silent --show-error \
    -X POST "$endpoint" \
    -H 'content-type: application/json' \
    -H 'accept: application/json, text/event-stream' \
    -H "authorization: Bearer $token" \
    --data "$payload" \
    >"$output"
  jq -e '.error == null and .result.isError != true' "$output" >/dev/null
}

start_server
initialize "$responses/initialize-first.json"

unauthorized_status=$(curl --silent --output "$responses/unauthorized.json" \
  --write-out '%{http_code}' \
  -X POST "$endpoint" \
  -H 'content-type: application/json' \
  -H 'accept: application/json, text/event-stream' \
  --data '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}')
test "$unauthorized_status" = 401

mcp_call \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"doc_new","arguments":{"name":"container-smoke","width":8,"height":8}}}' \
  "$responses/doc-new.json"
doc_id=$(jq -er '.result.structuredContent.doc_id | select(type == "string" and length > 0)' \
  "$responses/doc-new.json")

# Replace the server, retaining only the named /data volume. Subsequent reads
# therefore prove both a real MCP mutation and persistence across containers.
docker rm -f "$container" >/dev/null
start_server
initialize "$responses/initialize-second.json"

mcp_call \
  '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"list_docs","arguments":{}}}' \
  "$responses/list-docs.json"
jq -e --arg doc_id "$doc_id" \
  '.result.structuredContent.documents | any(.doc_id == $doc_id)' \
  "$responses/list-docs.json" >/dev/null

mcp_call \
  "{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"tools/call\",\"params\":{\"name\":\"doc_info\",\"arguments\":{\"doc_id\":\"$doc_id\"}}}" \
  "$responses/doc-info.json"
jq -e --arg doc_id "$doc_id" \
  '.result.structuredContent | .doc_id == $doc_id and .w == 8 and .h == 8' \
  "$responses/doc-info.json" >/dev/null

echo "container smoke: authenticated MCP mutation persisted as $doc_id"
