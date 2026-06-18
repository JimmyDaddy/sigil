# OpenAI-Compatible Provider

[文档首页](README.md) · [Provider 指南](providers.md) · [配置](configuration.md) · [DeepSeek](provider-deepseek.md) · [Anthropic](provider-anthropic.md) · [Gemini](provider-gemini.md) · [English](../en/provider-openai-compatible.md)

当你的 endpoint 实现 Chat Completions streaming 形态时，使用 OpenAI-compatible provider。这包括 OpenAI `/v1` endpoint 和兼容网关。

## 最小配置

临时本地使用：

```bash
export SIGIL_OPENAI_COMPATIBLE_API_KEY="sk-..."
sigil
```

可复用配置：

```toml
[agent]
provider = "openai_compat"
model = "gpt-4.1"
tool_timeout_secs = 30

[providers.openai_compat]
base_url = "https://api.openai.com/v1"
model = "gpt-4.1"
# 优先使用 SIGIL_OPENAI_COMPATIBLE_API_KEY 或 OPENAI_API_KEY。
# api_key = "sk-..."
organization = "org_..."
project = "proj_..."
request_timeout_secs = 120
```

完整起点模板见 [openai-compatible.toml](../examples/config/openai-compatible.toml)。

## 认证

Sigil 按这个顺序解析 OpenAI-compatible 认证：

1. `SIGIL_OPENAI_COMPATIBLE_API_KEY`
2. `OPENAI_API_KEY`
3. `[providers.openai_compat].api_key`

`organization` 和 `project` 只在 endpoint 或账号要求时才需要。

## 环境变量覆盖

| 变量 | 覆盖 |
| --- | --- |
| `SIGIL_OPENAI_COMPATIBLE_MODEL` | `[providers.openai_compat].model` |
| `SIGIL_OPENAI_COMPATIBLE_BASE_URL` | `[providers.openai_compat].base_url` |
| `SIGIL_OPENAI_COMPATIBLE_REQUEST_TIMEOUT_SECS` | `[providers.openai_compat].request_timeout_secs` |

这些覆盖适合 CI 和本地实验，不需要修改 `sigil.toml`。

## 行为说明

该 provider 会把 Sigil 的 provider-neutral messages、tool specs、streamed tool calls、usage 和可选 `system_fingerprint` 映射到 Chat Completions-compatible API。

它不提供 DeepSeek-only prefix/FIM、reasoning replay、strict tools mode 或 beta endpoint 设置。如果需要这些能力，请使用 [DeepSeek provider](provider-deepseek.md)。

## 验证

运行：

```bash
sigil doctor
```

确认 `[agent].provider` 是 `openai_compat`，base URL 包含预期 `/v1` 路径，并且 key 来源是预期环境变量。

## 常见问题

| 现象 | 检查 |
| --- | --- |
| 404 或 route 错误 | 确认 `base_url` 指向 Chat Completions-compatible `/v1` root。 |
| 设置了 `OPENAI_API_KEY` 仍认证失败 | 检查是否存在旧的 `SIGIL_OPENAI_COMPATIBLE_API_KEY` 且优先级更高。 |
| Tool calls 不被接受 | 确认所选 endpoint/model 支持 streamed tool calls。 |
| 计费账号或 project 不对 | 检查 `organization`、`project` 和 provider 控制台设置。 |
