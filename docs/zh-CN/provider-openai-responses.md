<!-- public-doc-role: provider-openai-responses; authority: provider-specific-setup; sections: minimal-setup,authentication,options-and-visible-limits,verify,common-problems; cta: return-providers -->

# OpenAI Responses Provider

[Provider 指南](providers.md) · [OpenAI-compatible](provider-openai-compatible.md) · [English](../en/provider-openai-responses.md)

## 最小设置

```bash
export SIGIL_OPENAI_RESPONSES_API_KEY="sk-..."
sigil
```

```toml
[agent]
provider = "openai_responses"
model = "gpt-4.1"

[providers.openai_responses]
base_url = "https://api.openai.com/v1"
```

可复制文件见 [openai-responses.toml](../examples/config/openai-responses.toml)。

## 认证

`SIGIL_OPENAI_RESPONSES_API_KEY` 优先于 `[providers.openai_responses].api_key`。`organization` 与 `project` 是可选 account 字段。

## 选项与可见限制

`SIGIL_OPENAI_RESPONSES_BASE_URL` 临时覆盖 `base_url`。该 provider 使用 Responses route，不是 Chat Completions。Background request 与 provider-hosted tool 未启用。

只有 Sigil 识别为支持图片的 model ID 才能接收附件；未知名称与 alias 会在发送前被拒绝。对 official endpoint 和受支持的 dated snapshot，一次发生在输出前的 context-window rejection 可能触发一次精简后重试；compatible endpoint、alias、恢复的 session 和重复失败不会使用该路径。

## 验证

运行 `sigil doctor`，确认 `openai_responses`、`/v1` base URL、model 和凭据来源。

## 常见问题

- 404：确认服务提供 `/v1/responses`，而不只提供 Chat Completions。
- 认证失败：检查环境变量或 config fallback。
- Stream 提前结束：确认 endpoint 发出 completed Responses event。
- Tool 或图片输入失败：确认所选 model 支持该输入。

<!-- public-doc-cta: return-providers -->
下一步：[返回 Provider 指南](providers.md)。
