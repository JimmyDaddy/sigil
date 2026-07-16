# Anthropic Provider

[文档首页](README.md) · [Provider 指南](providers.md) · [配置](configuration.md) · [DeepSeek](provider-deepseek.md) · [OpenAI-compatible](provider-openai-compatible.md) · [Gemini](provider-gemini.md) · [English](../en/provider-anthropic.md)

当你希望 Sigil 通过 Anthropic Messages streaming API 调用 Claude 模型时，选择 Anthropic provider。

## 最小配置

临时本地使用：

```bash
export SIGIL_ANTHROPIC_API_KEY="sk-ant-..."
sigil
```

可复用配置：

```toml
[agent]
provider = "anthropic"
model = "claude-sonnet-4-5"
tool_timeout_secs = 30

[model_request]
request_timeout_secs = 120
stream_idle_timeout_secs = 180

[providers.anthropic]
base_url = "https://api.anthropic.com"
# 优先使用 SIGIL_ANTHROPIC_API_KEY。
# api_key = "sk-ant-..."
anthropic_version = "2023-06-01"
max_tokens = 4096
beta_headers = []
```

完整起点模板见 [anthropic.toml](../examples/config/anthropic.toml)。

## 认证

Sigil 按这个顺序解析 Anthropic 认证：

1. `SIGIL_ANTHROPIC_API_KEY`
2. `[providers.anthropic].api_key`

本地和 CI 优先使用环境变量。不要提交包含明文 `api_key` 的配置文件。

## 环境变量覆盖

| 变量 | 覆盖 |
| --- | --- |
| `SIGIL_ANTHROPIC_BASE_URL` | `[providers.anthropic].base_url` |
| `SIGIL_ANTHROPIC_VERSION` | `[providers.anthropic].anthropic_version` |
| `SIGIL_ANTHROPIC_MAX_TOKENS` | `[providers.anthropic].max_tokens` |

## 行为说明

Sigil 会为你处理 Anthropic 的请求格式、流式回复、tool result、usage 和增量工具输入。Anthropic 专属选项留在本文；正常的 tool approval、隐私与 session 工作流保持一致。

只有明确识别的 Claude model id 及其已接受的日期变体才能使用图片附件。未知名称和未识别 alias 会在 provider transport 前失败。输入方式、本地上限、cache 行为与 resume 建议见[图片附件](user-guide.md#图片附件)。

只有在你明确知道 Anthropic feature 或 endpoint 需要时，才使用 `beta_headers`。

## 验证

运行：

```bash
sigil doctor
```

检查 provider 名称、model、`anthropic_version`、base URL 和 API key 来源。

## 常见问题

| 现象 | 检查 |
| --- | --- |
| 请求因 version/header 被拒绝 | 确认 `anthropic_version` 和 `beta_headers`。 |
| 输出过早停止 | 检查 `max_tokens` 和模型限制。 |
| 认证用了错误 key | 确认 `SIGIL_ANTHROPIC_API_KEY` 或 `[providers.anthropic].api_key` 是预期值。 |
| Tool-use 行为和其他 provider 不同 | 对比 provider 支持情况，测试期间保持 permission policy 不变。 |
