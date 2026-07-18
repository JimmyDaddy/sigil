<!-- public-doc-role: providers; authority: provider-selection-authority; sections: choose-a-provider,authentication-priority,copyable-starting-points,troubleshooting-path; cta: open-provider-guide -->

# 模型服务指南

[文档首页](README.md) · [配置](configuration.md) · [English](../en/providers.md)

先在这里选择模型服务，再进入对应页面完成设置并了解当前限制。权限、会话和工具的通用行为在所有模型服务间保持一致，不需要重复学习。

## 选择模型服务

| 模型服务 | 适合场景 | 图片输入 | 配置值 |
| --- | --- | --- | --- |
| [DeepSeek](provider-deepseek.md) | 快速设置的默认路径，以及 DeepSeek 专用选项 | 不支持 | `deepseek` |
| [OpenAI-compatible](provider-openai-compatible.md) | 兼容 Chat Completions 的 `/v1` 网关 | 不支持 | `openai_compat` |
| [OpenAI Responses](provider-openai-responses.md) | 使用 OpenAI Responses 接口 | 识别到的模型 ID | `openai_responses` |
| [Anthropic](provider-anthropic.md) | 通过 Anthropic Messages 使用 Claude | 识别到的 Claude ID | `anthropic` |
| [Gemini](provider-gemini.md) | Gemini 与函数调用 | 识别到的 Gemini ID | `gemini` |

首次使用时，最快的方式是跟随快速设置。需要在本机或 CI 中重复使用相同设置时，再改用手写配置。

## 认证优先级

优先使用环境变量。配置中的 `api_key` 备用值会以明文写入 `sigil.toml`。

| 模型服务 | 环境变量 | 配置备用值 |
| --- | --- | --- |
| DeepSeek | `SIGIL_API_KEY` | `[providers.deepseek].api_key` |
| OpenAI-compatible | `SIGIL_OPENAI_COMPATIBLE_API_KEY` | `[providers.openai_compat].api_key` |
| OpenAI Responses | `SIGIL_OPENAI_RESPONSES_API_KEY` | `[providers.openai_responses].api_key` |
| Anthropic | `SIGIL_ANTHROPIC_API_KEY` | `[providers.anthropic].api_key` |
| Gemini | `SIGIL_GEMINI_API_KEY` | `[providers.gemini].api_key` |

修改凭据后运行 `sigil doctor`。它会报告来源，但不打印值。

## 可复制起点

模板位于 [`docs/examples/config`](../examples/config)。使用前请检查具体模型、基础 URL、凭据来源和权限设置。

## 排障路径

依次检查 `[agent].provider`、所选模型、对应的配置区块、基础 URL、启动 Shell 能否读取凭据，以及模型服务的专用限制。排障期间保持 `permission.mode = "manual"`；如果问题并非某个模型服务特有，再进入[故障排查](troubleshooting.md)。

<!-- public-doc-cta: open-provider-guide -->
下一步：[设置 DeepSeek，或选择其他模型服务](provider-deepseek.md)。
