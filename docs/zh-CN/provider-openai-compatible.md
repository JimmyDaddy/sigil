<!-- public-doc-role: provider-openai-compatible; authority: provider-specific-setup; sections: minimal-setup,authentication,options-and-visible-limits,verify,common-problems; cta: return-providers -->

# 接入 OpenAI-compatible 服务

[模型服务指南](providers.md) · [配置](configuration.md) · [English](../en/provider-openai-compatible.md)

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

`SIGIL_OPENAI_COMPATIBLE_API_KEY` 优先于 `[providers.openai_compat].api_key`。`organization` 与 `project` 是可选的账户字段。

## 选项与可见限制

`SIGIL_OPENAI_COMPATIBLE_BASE_URL` 可以临时覆盖 `base_url`。端点与模型必须支持流式 Chat Completions 和工具调用。

即使某个服务提供自己的多模态扩展，Sigil 也不会通过通用兼容端点接收图片附件。DeepSeek 专用的 FIM 和严格工具设置同样不适用。

## 验证

运行 `sigil doctor`，确认 `openai_compat`、预期的 `/v1` 基础 URL、具体模型和凭据来源。

## 常见问题

- 404：让 `base_url` 指向兼容服务的 `/v1` 根路径。
- 认证失败：检查环境变量或配置中的备用凭据。
- 工具调用失败：确认端点与模型支持流式工具调用。
- 账户错误：检查 `organization`、`project` 和服务商控制台设置。

<!-- public-doc-cta: return-providers -->
下一步：[返回模型服务指南](providers.md)。
