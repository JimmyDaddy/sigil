<!-- public-doc-role: providers; authority: provider-selection-authority; sections: choose-a-provider,authentication-priority,copyable-starting-points,troubleshooting-path; cta: open-provider-guide -->

# Provider 指南

[文档首页](README.md) · [配置](configuration.md) · [English](../en/providers.md)

先在这里选择模型服务，再进入对应页面完成设置并查看可见限制。共享权限、session 和工具行为不需要在每个 provider 页面重复学习。

## 选择 Provider

| Provider | 适合场景 | 图片输入 | 配置值 |
| --- | --- | --- | --- |
| [DeepSeek](provider-deepseek.md) | 默认 Quick Setup 路径和 DeepSeek 专项选项 | 不支持 | `deepseek` |
| [OpenAI-compatible](provider-openai-compatible.md) | Chat Completions-compatible `/v1` gateway | 不支持 | `openai_compat` |
| [OpenAI Responses](provider-openai-responses.md) | OpenAI Responses model | 识别到的 model ID | `openai_responses` |
| [Anthropic](provider-anthropic.md) | 通过 Anthropic Messages 使用 Claude | 识别到的 Claude ID | `anthropic` |
| [Gemini](provider-gemini.md) | Gemini 与 function calling | 识别到的 Gemini ID | `gemini` |

首次使用最短路径是 Quick Setup。需要可重复的本机或 CI 默认值时，再使用手写配置。

## 认证优先级

优先使用环境变量。Config 中的 `api_key` fallback 会以明文写入 `sigil.toml`。

| Provider | 环境变量 | Config fallback |
| --- | --- | --- |
| DeepSeek | `SIGIL_API_KEY` | `[providers.deepseek].api_key` |
| OpenAI-compatible | `SIGIL_OPENAI_COMPATIBLE_API_KEY` | `[providers.openai_compat].api_key` |
| OpenAI Responses | `SIGIL_OPENAI_RESPONSES_API_KEY` | `[providers.openai_responses].api_key` |
| Anthropic | `SIGIL_ANTHROPIC_API_KEY` | `[providers.anthropic].api_key` |
| Gemini | `SIGIL_GEMINI_API_KEY` | `[providers.gemini].api_key` |

修改凭据后运行 `sigil doctor`。它会报告来源，但不打印值。

## 可复制起点

模板位于 [`docs/examples/config`](../examples/config)。使用前检查 model、base URL、凭据来源和权限设置。

## 排障路径

依次检查 `[agent].provider`、所选 model、provider block、base URL、启动 shell 中的凭据可见性和 provider 专项限制。排障期间保持 `permission.mode = "manual"`，共享症状再进入[故障排查](troubleshooting.md)。

<!-- public-doc-cta: open-provider-guide -->
下一步：[设置 DeepSeek 或选择其他 Provider](provider-deepseek.md)。
