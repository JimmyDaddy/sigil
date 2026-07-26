<!-- public-doc-role: provider-openai-responses; authority: provider-specific-setup; sections: minimal-setup,authentication,options-and-visible-limits,verify,common-problems; cta: return-providers -->

# 接入 OpenAI Responses

[模型服务指南](providers.md) · [OpenAI-compatible](provider-openai-compatible.md) · [English](../en/provider-openai-responses.md)

## 最小设置

```bash
export SIGIL_OPENAI_RESPONSES_API_KEY="sk-..."
sigil
```

```toml
config_version = 2

[agent]
connection = "openai-default"
model = "gpt-4.1"

[connections.openai-default]
label = "OpenAI"
provider = "openai"
protocol = "responses"
base_url = "https://api.openai.com/v1"
credential = { source = "environment", name = "SIGIL_OPENAI_RESPONSES_API_KEY" }
```

可复制文件见 [openai-responses.toml](../examples/config/openai-responses.toml)。

## 认证

示例只把当前连接绑定到 `SIGIL_OPENAI_RESPONSES_API_KEY`。也可以选择安全凭据存储；`sigil.toml`、模型缓存和会话文件都不包含 secret 值。`organization` 与 `project` 是可选连接选项。

## 选项与可见限制

这条连接使用 Responses 路由，而不是 Chat Completions。端点与账户选项都属于当前连接，另一个 OpenAI 或兼容连接不会成为回退来源。后台请求和服务商托管工具尚未启用。

只有被 Sigil 识别为支持图片的模型 ID 才能接收附件；未知名称和别名会在发送前被拒绝。对于官方端点和受支持的带日期模型版本，如果请求在输出前因上下文窗口不足而被拒绝，Sigil 可能在精简上下文后重试一次。兼容端点、别名、恢复的会话和重复失败都不会使用这条路径。

## 验证

运行 `sigil doctor`，确认 `default=openai-default/gpt-4.1`、`responses` 协议、`/v1` 端点、凭据来源和就绪状态。

## 常见问题

- 404：确认服务提供 `/v1/responses`，而不只提供 Chat Completions。
- 认证失败：检查绑定的环境变量，或在 `/config` 中修复当前连接；Sigil 不会回退到其他连接。
- 流式响应提前结束：确认端点会发送 completed Responses 事件。
- 工具或图片输入失败：确认所选模型支持该输入。

<!-- public-doc-cta: return-providers -->
下一步：[返回模型服务指南](providers.md)。
