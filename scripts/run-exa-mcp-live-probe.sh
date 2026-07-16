#!/usr/bin/env bash
set -euo pipefail

# Release-evidence probe for the pinned bundled Exa MCP contract. This is intentionally not a CI
# check: it performs a real external query only after an operator has opted in explicitly.
if [[ "${SIGIL_EXA_LIVE_PROBE:-}" != "1" ]]; then
  echo "refusing live Exa probe; rerun with SIGIL_EXA_LIVE_PROBE=1" >&2
  exit 64
fi

endpoint="https://mcp.exa.ai/mcp"
probe_query="Sigil MCP release compatibility probe"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

post_json() {
  local body="$1"
  local response="$2"
  local headers="$3"
  local session_id="${4:-}"
  local -a request_args=(
    --silent --show-error --fail-with-body --proto '=https' --tlsv1.2
    --connect-timeout 10 --max-time 45 --max-redirs 0
    -D "${headers}" -o "${response}"
    -H 'Accept: application/json, text/event-stream'
    -H 'Content-Type: application/json'
    -H 'User-Agent: Sigil/0.0.1-alpha.4 (release-evidence)'
  )
  if [[ -n "${session_id}" ]]; then
    request_args+=(-H "Mcp-Session-Id: ${session_id}")
  fi
  curl "${request_args[@]}" --data "${body}" "${endpoint}"
}

session_id_from_headers() {
  awk 'tolower($0) ~ /^mcp-session-id:/ { sub(/^[^:]*:[[:space:]]*/, ""); sub(/\r$/, ""); print; exit }' "$1"
}

response_contains() {
  local response="$1"
  local needle="$2"
  ruby -e '
    body = File.binread(ARGV.fetch(0))
    needle = ARGV.fetch(1)
    exit(body.include?(needle) ? 0 : 1)
  ' "${response}" "${needle}"
}

validate_tools_contract() {
  local response="$1"
  ruby -r json -r digest -e '
    body = File.binread(ARGV.fetch(0))
    payloads = [body]
    payloads.concat(body.lines.filter_map { |line| line.start_with?("data:") ? line.delete_prefix("data:").strip : nil })
    message = payloads.filter_map { |payload| JSON.parse(payload) rescue nil }
      .find { |candidate| candidate.dig("result", "tools").is_a?(Array) }
    abort("tools/list response did not contain a JSON-RPC tools array") unless message
    tool = message.dig("result", "tools").find { |candidate| candidate["name"] == "web_search_exa" }
    abort("tools/list response did not contain web_search_exa") unless tool
    schema = tool["inputSchema"]
    properties = schema.is_a?(Hash) ? schema["properties"] : nil
    required = schema.is_a?(Hash) ? schema["required"] : nil
    valid = schema.is_a?(Hash) && schema["type"] == "object" &&
      properties.is_a?(Hash) && properties.dig("query", "type") == "string" &&
      properties.dig("numResults", "type") == "number" &&
      required.is_a?(Array) && required.include?("query")
    unless valid
      warn(JSON.pretty_generate(schema))
      abort("web_search_exa inputSchema no longer satisfies the pinned query/numResults contract")
    end
    puts "web_search_exa schema sha256=#{Digest::SHA256.hexdigest(JSON.generate(schema))}"
  ' "${response}"
}

initialize_headers="${tmp_dir}/initialize.headers"
initialize_response="${tmp_dir}/initialize.response"
post_json \
  '{"jsonrpc":"2.0","id":"sigil-probe-initialize","method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"Sigil","version":"0.0.1-alpha.4"}}}' \
  "${initialize_response}" "${initialize_headers}"

response_contains "${initialize_response}" '"jsonrpc"'
session_id="$(session_id_from_headers "${initialize_headers}")"
if [[ -z "${session_id}" ]]; then
  echo "initialize response did not provide Mcp-Session-Id" >&2
  exit 1
fi

initialized_headers="${tmp_dir}/initialized.headers"
initialized_response="${tmp_dir}/initialized.response"
post_json \
  '{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}' \
  "${initialized_response}" "${initialized_headers}" "${session_id}"

tools_headers="${tmp_dir}/tools.headers"
tools_response="${tmp_dir}/tools.response"
post_json \
  '{"jsonrpc":"2.0","id":"sigil-probe-tools-list","method":"tools/list","params":{}}' \
  "${tools_response}" "${tools_headers}" "${session_id}"
response_contains "${tools_response}" 'web_search_exa'
validate_tools_contract "${tools_response}"

call_headers="${tmp_dir}/call.headers"
call_response="${tmp_dir}/call.response"
post_json \
  "{\"jsonrpc\":\"2.0\",\"id\":\"sigil-probe-web-search\",\"method\":\"tools/call\",\"params\":{\"name\":\"web_search_exa\",\"arguments\":{\"query\":\"${probe_query}\",\"numResults\":1}}}" \
  "${call_response}" "${call_headers}" "${session_id}"
response_contains "${call_response}" '"jsonrpc"'

echo "Exa MCP live probe passed: initialize, tools/list, web_search_exa"
