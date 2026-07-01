# Gemini Provider

[文档首页](README.md) · [Provider 指南](providers.md) · [配置](configuration.md) · [DeepSeek](provider-deepseek.md) · [OpenAI-compatible](provider-openai-compatible.md) · [Anthropic](provider-anthropic.md) · [English](../en/provider-gemini.md)

当你希望 Sigil 通过 Google `streamGenerateContent` API 调用 Gemini 模型时，选择 Gemini provider。

## 最小配置

临时本地使用：

```bash
export SIGIL_GEMINI_API_KEY="..."
sigil
```

可复用配置：

```toml
[agent]
provider = "gemini"
model = "gemini-2.5-pro"
tool_timeout_secs = 30

[model_request]
request_timeout_secs = 120
stream_idle_timeout_secs = 180

[providers.gemini]
base_url = "https://generativelanguage.googleapis.com/v1beta"
model = "gemini-2.5-pro"
# 优先使用 SIGIL_GEMINI_API_KEY、GEMINI_API_KEY 或 GOOGLE_API_KEY。
# api_key = "..."
```

完整起点模板见 [gemini.toml](../examples/config/gemini.toml)。

## 认证

Sigil 按这个顺序解析 Gemini 认证：

1. `SIGIL_GEMINI_API_KEY`
2. `GEMINI_API_KEY`
3. `GOOGLE_API_KEY`
4. `[providers.gemini].api_key`

如果希望 Sigil 使用专属凭据，同时不影响同一个 shell 里的其他 Google 工具，优先使用 `SIGIL_GEMINI_API_KEY`。

## 环境变量覆盖

| 变量 | 覆盖 |
| --- | --- |
| `SIGIL_GEMINI_MODEL` | `[providers.gemini].model` |
| `SIGIL_GEMINI_BASE_URL` | `[providers.gemini].base_url` |

## 行为说明

Sigil 会在 provider crate 内把 provider-neutral messages、tool specs、function calls、function responses、usage 和 block reasons 映射到 Gemini 协议细节。Gemini 专属的 `systemInstruction`、`functionDeclarations` 和 `functionResponse` 细节不进入 `sigil-kernel`。

Gemini model 名称和 endpoint 可用性可能随账号和区域变化。自动化使用时，请在配置里显式写明 model 名称。

## 验证

运行：

```bash
sigil doctor
```

检查 provider 名称、model、base URL、timeout 和 API key 来源。

## 常见问题

| 现象 | 检查 |
| --- | --- |
| 认证失败 | 确认 `SIGIL_GEMINI_API_KEY`、`GEMINI_API_KEY` 或 `GOOGLE_API_KEY` 哪一个对 `sigil` 进程可见。 |
| 找不到 model | 确认 Gemini model 名称和 endpoint version。 |
| Tool/function calls 失败 | 确认该 model 和 endpoint 对你的账号支持 function calling。 |
| 请求超时 | 检查网络，并考虑 `[model_request].stream_idle_timeout_secs`。 |
