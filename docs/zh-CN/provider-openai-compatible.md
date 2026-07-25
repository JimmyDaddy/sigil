<!-- public-doc-role: provider-openai-compatible; authority: provider-specific-setup; sections: minimal-setup,authentication,options-and-visible-limits,verify,common-problems; cta: return-providers -->

# 接入 OpenAI-compatible 服务

[模型服务指南](providers.md) · [配置](configuration.md) · [English](../en/provider-openai-compatible.md)

## 最小设置

```bash
export SIGIL_OPENAI_COMPATIBLE_API_KEY="sk-..."
sigil
```

```toml
config_version = 2

[agent]
connection = "custom-default"
model = "gpt-4.1"

[connections.custom-default]
label = "Custom endpoint"
provider = "custom"
protocol = "chat_completions"
base_url = "https://api.openai.com/v1"
credential = { source = "environment", name = "SIGIL_OPENAI_COMPATIBLE_API_KEY" }
```

可复制文件见 [openai-compatible.toml](../examples/config/openai-compatible.toml)。

## 认证

示例只把当前连接绑定到 `SIGIL_OPENAI_COMPATIBLE_API_KEY`。首次设置和 `/config` 也支持安全凭据存储；V2 会拒绝明文 `api_key` connection 字段。`organization` 与 `project` 是可选连接选项。

## 选项与可见限制

端点与模型必须支持流式 Chat Completions 和工具调用。每条自定义连接独立拥有 URL、协议、凭据与模型目录；Sigil 不会借用另一条连接的模型或凭据。

即使某个服务提供自己的多模态扩展，Sigil 也不会通过通用兼容端点接收图片附件。DeepSeek 专用的 FIM 和严格工具设置同样不适用。

## 验证

运行 `sigil doctor`，确认 `default=custom-default/gpt-4.1`、`chat_completions` 协议、预期 `/v1` 端点、凭据来源和就绪状态。

## 常见问题

- 404：让 `base_url` 指向兼容服务的 `/v1` 根路径。
- 认证失败：检查绑定的环境变量，或在 `/config` 中修复当前连接。
- 工具调用失败：确认端点与模型支持流式工具调用。
- 账户错误：检查 `organization`、`project` 和服务商控制台设置。

<!-- public-doc-cta: return-providers -->
下一步：[返回模型服务指南](providers.md)。
