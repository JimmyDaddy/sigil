<!-- public-doc-role: provider-openai-responses; authority: provider-specific-setup; sections: minimal-setup,authentication,options-and-visible-limits,verify,common-problems; cta: return-providers -->

# 接入 OpenAI Responses

[模型服务指南](providers.md) · [OpenAI-compatible](provider-openai-compatible.md) · [English](../en/provider-openai-responses.md)

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

`SIGIL_OPENAI_RESPONSES_API_KEY` 优先于 `[providers.openai_responses].api_key`。`organization` 与 `project` 是可选的账户字段。

## 选项与可见限制

`SIGIL_OPENAI_RESPONSES_BASE_URL` 可以临时覆盖 `base_url`。这一接入方式使用 Responses 路由，而不是 Chat Completions。后台请求和服务商托管工具尚未启用。

只有被 Sigil 识别为支持图片的模型 ID 才能接收附件；未知名称和别名会在发送前被拒绝。对于官方端点和受支持的带日期模型版本，如果请求在输出前因上下文窗口不足而被拒绝，Sigil 可能在精简上下文后重试一次。兼容端点、别名、恢复的会话和重复失败都不会使用这条路径。

## 验证

运行 `sigil doctor`，确认 `openai_responses`、`/v1` 基础 URL、具体模型和凭据来源。

## 常见问题

- 404：确认服务提供 `/v1/responses`，而不只提供 Chat Completions。
- 认证失败：检查环境变量或配置中的备用凭据。
- 流式响应提前结束：确认端点会发送 completed Responses 事件。
- 工具或图片输入失败：确认所选模型支持该输入。

<!-- public-doc-cta: return-providers -->
下一步：[返回模型服务指南](providers.md)。
