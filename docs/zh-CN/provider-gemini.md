<!-- public-doc-role: provider-gemini; authority: provider-specific-setup; sections: minimal-setup,authentication,options-and-visible-limits,verify,common-problems; cta: return-providers -->

# 接入 Gemini

[模型服务指南](providers.md) · [配置](configuration.md) · [English](../en/provider-gemini.md)

## 最小设置

```bash
export SIGIL_GEMINI_API_KEY="..."
sigil
```

```toml
config_version = 2

[agent]
connection = "gemini-default"
model = "gemini-2.5-pro"

[connections.gemini-default]
label = "Google Gemini"
provider = "gemini"
protocol = "generate_content"
base_url = "https://generativelanguage.googleapis.com/v1beta"
credential = { source = "environment", name = "SIGIL_GEMINI_API_KEY" }
```

可复制文件见 [gemini.toml](../examples/config/gemini.toml)。

## 认证

示例只把当前连接绑定到 `SIGIL_GEMINI_API_KEY`，不会改变其他 Google 工具使用的凭据。也可以选择安全凭据存储；`sigil.toml` 中只保存不透明凭据引用。

## 选项与可见限制

模型可用性可能因账户和区域而异；请明确设置 `[agent].connection` 与 `[agent].model`。第二个 Gemini 账户应使用独立连接，拥有自己的端点、凭据和模型目录。

图片只支持已识别的 Gemini 模型 ID。浮动的 `latest` 名称、未知 ID 和别名会在发送前被拒绝。

## 验证

运行 `sigil doctor`，确认 `default=gemini-default/gemini-2.5-pro`、端点、凭据来源和就绪状态。

## 常见问题

- 认证失败：检查启动 Shell 中的 `SIGIL_GEMINI_API_KEY`。
- 找不到模型：确认模型名称、端点版本、账户和区域。
- 函数调用失败：确认模型与端点支持函数调用。
- 超时：检查网络和模型请求超时设置。

<!-- public-doc-cta: return-providers -->
下一步：[返回模型服务指南](providers.md)。
