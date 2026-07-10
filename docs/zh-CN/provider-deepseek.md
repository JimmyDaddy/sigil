# DeepSeek Provider

[文档首页](README.md) · [Provider 指南](providers.md) · [配置](configuration.md) · [OpenAI-compatible](provider-openai-compatible.md) · [Anthropic](provider-anthropic.md) · [Gemini](provider-gemini.md) · [English](../en/provider-deepseek.md)

当你希望使用 Sigil 默认 Quick Setup 路径、DeepSeek chat 模型，以及 DeepSeek 专项 FIM 或 endpoint 设置时，选择 DeepSeek provider。

## 最小配置

临时本地使用时，在启动前设置 API key：

```bash
export SIGIL_API_KEY="sk-..."
sigil
```

需要可重复配置时，使用：

```toml
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
tool_timeout_secs = 30

[model_request]
request_timeout_secs = 120
stream_idle_timeout_secs = 180

[providers.deepseek]
base_url = "https://api.deepseek.com"
beta_base_url = "https://api.deepseek.com/beta"
anthropic_base_url = "https://api.deepseek.com/anthropic"
fim_model = "deepseek-v4-pro"
# 推荐优先使用 SIGIL_API_KEY；如果写在这里，会以 plaintext 保存。
# api_key = "sk-..."
user_id_strategy = "stable_per_end_user"
strict_tools_mode = "auto"
```

更短的模板见 [deepseek-basic.toml](../examples/config/deepseek-basic.toml)。

## 认证

Sigil 按这个顺序解析 DeepSeek 认证：

1. `SIGIL_API_KEY`
2. `[providers.deepseek].api_key`

本地 shell 和 CI 优先用 `SIGIL_API_KEY`。如果通过 `/config` 保存 `api_key`，它会以明文写入 `sigil.toml`；请确保该文件不会被提交。

## 常用字段

| 字段 | 用途 |
| --- | --- |
| `fim_model` | 可用时用于 DeepSeek FIM 相关流程的模型。 |
| `base_url` | DeepSeek 主 API endpoint。 |
| `beta_base_url` | 需要 beta 能力时使用的 DeepSeek beta endpoint。 |
| `anthropic_base_url` | 构建使用 DeepSeek Anthropic-compatible route 时的 endpoint。 |
| `strict_tools_mode` | DeepSeek 专项 tool strictness 行为。除非明确需要覆盖，否则使用 `auto`。 |
| `user_id_strategy` | Sigil 向 provider 提供稳定 user identifier 的方式。 |

TUI `/config` 只暴露高频字段，例如 `model`、`api_key`、`base_url` 和 `fim_model`。低频 DeepSeek 字段保留给配置文件或环境变量。

## 环境变量覆盖

| 变量 | 覆盖配置 |
| --- | --- |
| `SIGIL_BASE_URL` | `[providers.deepseek].base_url` |
| `SIGIL_BETA_BASE_URL` | `[providers.deepseek].beta_base_url` |
| `SIGIL_ANTHROPIC_BASE_URL` | `[providers.deepseek].anthropic_base_url` |
| `SIGIL_FIM_MODEL` | `[providers.deepseek].fim_model` |
| `SIGIL_USER_ID_STRATEGY` | `[providers.deepseek].user_id_strategy` |
| `SIGIL_STRICT_TOOLS_MODE` | `[providers.deepseek].strict_tools_mode` |

认证环境变量 `SIGIL_API_KEY` 见上方的[认证](#认证)章节。

## 验证

运行：

```bash
sigil doctor
```

确认报告中显示 `deepseek`、预期 model 和预期 API key 来源。报告不会打印密钥值。

## 常见问题

| 现象 | 检查 |
| --- | --- |
| Sigil 又进入 setup | 确认配置查找顺序找到了预期 `sigil.toml`。 |
| 认证失败 | 确认 `SIGIL_API_KEY` 设置在启动 `sigil` 的同一个 shell 中。 |
| 使用了错误 model | 检查 `[agent].model` 和 role-specific task model override。 |
| 响应慢或中断 | 检查网络，并考虑 `[model_request].stream_idle_timeout_secs`。 |
| FIM 行为不可用 | 确认 `fim_model` 已配置且 endpoint 支持。 |
