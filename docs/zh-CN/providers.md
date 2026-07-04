# Sigil Provider 指南

[文档首页](README.md) · [配置](configuration.md) · [DeepSeek](provider-deepseek.md) · [OpenAI-compatible](provider-openai-compatible.md) · [Anthropic](provider-anthropic.md) · [Gemini](provider-gemini.md) · [English](../en/providers.md)

Sigil 把 provider 选择和其他用户工作流拆开。TUI、审批、工具、session、MCP 和 code-intelligence 行为保持一致；provider 页面只说明模型 endpoint、认证和 provider 专项配置。

## 选择 Provider

| Provider | 最适合 | Config value | 指南 |
| --- | --- | --- | --- |
| DeepSeek | 默认 Quick Setup 路径、DeepSeek chat、FIM 和 DeepSeek 专项 endpoint。 | `deepseek` | [DeepSeek provider](provider-deepseek.md) |
| OpenAI-compatible | OpenAI 或兼容 Chat Completions `/v1` 的网关。 | `openai_compat` | [OpenAI-compatible provider](provider-openai-compatible.md) |
| Anthropic | 通过 Anthropic Messages streaming 使用 Claude 模型。 | `anthropic` | [Anthropic provider](provider-anthropic.md) |
| Gemini | 通过 `streamGenerateContent` 和 function calling 使用 Gemini 模型。 | `gemini` | [Gemini provider](provider-gemini.md) |

第一次使用时，直接运行 `sigil` 并完成 Quick Setup。需要可重复本地默认值、CI 自动化，或当前 Quick Setup 未覆盖的 provider 时，再手写配置。

## Provider 选择方式

设置 `[agent].provider`，并配置对应 `[providers.*]` 区块：

```toml
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
tool_timeout_secs = 30

[providers.deepseek]
# Provider 区块只放 endpoint、认证和 provider 专项字段。
```

`[agent].model` 是唯一聊天模型配置。计划任务中的 planner、executor 或 subagent role 仍可单独覆盖继承到的 agent provider/model。

## 认证优先级

凭据优先使用环境变量。明文 `api_key` 字段仍然支持本地私有配置，但当 key 只来自配置文件时，`doctor` 会给出 warning。

| Provider | 环境变量 key | 配置文件备用 |
| --- | --- | --- |
| DeepSeek | `SIGIL_API_KEY` | `[providers.deepseek].api_key` |
| OpenAI-compatible | `SIGIL_OPENAI_COMPATIBLE_API_KEY` | `[providers.openai_compat].api_key` |
| Anthropic | `SIGIL_ANTHROPIC_API_KEY` | `[providers.anthropic].api_key` |
| Gemini | `SIGIL_GEMINI_API_KEY` | `[providers.gemini].api_key` |

修改凭据后运行：

```bash
sigil doctor
```

在 TUI 中，`/doctor` 会把同一份 provider 和 key-source 检查渲染到 transcript，且不会打印密钥值。

## 可复制起点

配置模板位于 [docs/examples/config](../examples/config)：

- [deepseek-basic.toml](../examples/config/deepseek-basic.toml)
- [openai-compatible.toml](../examples/config/openai-compatible.toml)
- [anthropic.toml](../examples/config/anthropic.toml)
- [gemini.toml](../examples/config/gemini.toml)

把它们当作起点使用，然后在真实 workspace 中运行前检查 model 名称、base URL、key 来源和 permission policy。

## 行为边界

Provider 专项行为留在 provider 配置和 provider crate 内。共享 Sigil 工作流保持 provider-neutral：

- 工具调用仍然走同一套 approval 和 preview 流程。
- Session 和 control records 仍然 append-only。
- MCP trust 和 secret-egress policy 不随 provider 改变。
- `sigil-kernel` 不应出现 DeepSeek、OpenAI、Anthropic 或 Gemini 专属公共 API 术语。

## 排障路径

Provider 调用失败时：

1. 运行 `sigil doctor`，检查 provider 名称、model、base URL 和 API key 来源。
2. 确认 `[agent].provider` 和配置的 `[providers.*]` 区块一致。
3. 确认目标环境变量在启动 `sigil` 的同一个 shell 中可见。
4. 到 provider 专页检查 endpoint 和 timeout 字段。
5. 排障期间保持 `[permission].default_mode = "ask"`，让写操作保持可见。
