# OpenAI Responses Provider

[文档首页](README.md) · [Provider 指南](providers.md) · [配置](configuration.md) · [DeepSeek](provider-deepseek.md) · [OpenAI-compatible](provider-openai-compatible.md) · [Anthropic](provider-anthropic.md) · [Gemini](provider-gemini.md) · [English](../en/provider-openai-responses.md)

当所选模型由 OpenAI Responses API（`/v1/responses`）提供时，使用此 provider。它与 [OpenAI-compatible provider](provider-openai-compatible.md) 不同：后者使用 Chat Completions 协议，面向兼容网关。

## 最小配置

临时本地使用：

```bash
export SIGIL_OPENAI_RESPONSES_API_KEY="sk-..."
sigil
```

可复用配置：

```toml
[agent]
provider = "openai_responses"
model = "gpt-4.1"
tool_timeout_secs = 30

[model_request]
request_timeout_secs = 120
stream_idle_timeout_secs = 180

[providers.openai_responses]
base_url = "https://api.openai.com/v1"
# 优先使用 SIGIL_OPENAI_RESPONSES_API_KEY。
# api_key = "sk-..."
organization = "org_..."
project = "proj_..."
```

完整起点模板见 [openai-responses.toml](../examples/config/openai-responses.toml)。

## 认证

Sigil 按这个顺序解析 Responses 认证：

1. `SIGIL_OPENAI_RESPONSES_API_KEY`
2. `[providers.openai_responses].api_key`

`organization` 和 `project` 只在账号要求时才需要。

## 环境变量覆盖

| 变量 | 覆盖 |
| --- | --- |
| `SIGIL_OPENAI_RESPONSES_BASE_URL` | `[providers.openai_responses].base_url` |

## 行为说明

该 provider 从 Responses event stream 输出 text、已支持的 reasoning delta、tool call 和 usage。每个完成的原生 output-item array 会作为不解释的 provider state 保存，并在后续请求中原样替换对应 assistant turn；这可以保留 encrypted reasoning content 等 provider 私有 item，而不会改动 Chat Completions provider 的契约。

普通请求使用完整本地 session context，不使用远端 response-handle continuation。此 provider 未启用 background request 与 provider-hosted tool。原生 compact endpoint 不是用户操作。

受控 overflow recovery 与 Context Compaction V2 apply 一同处于冻结状态。修复正确性问题期间，它不会计数、压缩或重试请求。重新启用后，它仍只会限于官方 `https://api.openai.com/v1` endpoint 与精确的 `gpt-4.1-2025-04-14` snapshot：provider 确认 context-window rejection，且尚未产生 output 或 side effect 时才可能进入；alias、兼容 endpoint、普通错误、计数失败与恢复后的 session 仍将被排除。

## 验证

运行：

```bash
sigil doctor
```

确认 `[agent].provider` 是 `openai_responses`，base URL 包含预期 `/v1` 路径，且 key 来源是 `SIGIL_OPENAI_RESPONSES_API_KEY` 或本地配置。

## 常见问题

| 现象 | 检查 |
| --- | --- |
| 404 或 route 错误 | 确认服务在配置的 `/v1` root 下暴露 Responses route，而不是只支持 Chat Completions。 |
| 认证失败 | 确认已设置 `SIGIL_OPENAI_RESPONSES_API_KEY` 或 `[providers.openai_responses].api_key`。 |
| stream 在最终回复前结束 | provider 要求终结 `response.completed` event；检查 endpoint 或网关的 SSE 兼容性。 |
| Tool call 不被接受 | 确认所选模型支持 Responses function tool。 |
