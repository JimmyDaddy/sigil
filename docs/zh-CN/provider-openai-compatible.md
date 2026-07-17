<!-- public-doc-role: provider-openai-compatible; authority: provider-specific-setup; sections: minimal-setup,authentication,options-and-visible-limits,verify,common-problems; cta: return-providers -->

# OpenAI-Compatible Provider

[Provider 指南](providers.md) · [配置](configuration.md) · [English](../en/provider-openai-compatible.md)

## 最小设置

```bash
export SIGIL_OPENAI_COMPATIBLE_API_KEY="sk-..."
sigil
```

```toml
[agent]
provider = "openai_compat"
model = "gpt-4.1"

[providers.openai_compat]
base_url = "https://api.openai.com/v1"
```

可复制文件见 [openai-compatible.toml](../examples/config/openai-compatible.toml)。

## 认证

`SIGIL_OPENAI_COMPATIBLE_API_KEY` 优先于 `[providers.openai_compat].api_key`。`organization` 与 `project` 是可选 account 字段。

## 选项与可见限制

`SIGIL_OPENAI_COMPATIBLE_BASE_URL` 临时覆盖 `base_url`。Endpoint 与 model 必须支持 streamed Chat Completions 和 tool call。

即使某个服务提供自己的 multimodal extension，Sigil 也不会通过通用 compatible endpoint 接收图片附件。DeepSeek 专项 FIM 与 strict-tool 设置同样不适用。

## 验证

运行 `sigil doctor`，确认 `openai_compat`、预期 `/v1` base URL、model 和凭据来源。

## 常见问题

- 404：让 `base_url` 指向 compatible `/v1` root。
- 认证失败：检查环境变量或 config fallback。
- Tool call 失败：确认 endpoint 与 model 支持 streamed tool call。
- Account 错误：检查 `organization`、`project` 与 provider dashboard 设置。

<!-- public-doc-cta: return-providers -->
下一步：[返回 Provider 指南](providers.md)。
