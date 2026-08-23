<!-- public-doc-role: provider-deepseek; authority: provider-specific-setup; sections: minimal-setup,authentication,options-and-visible-limits,verify,common-problems; cta: return-providers -->

# 接入 DeepSeek

[模型服务指南](providers.md) · [配置](configuration.md) · [English](../en/provider-deepseek.md)

## 最小设置

```bash
export SIGIL_API_KEY="sk-..."
sigil
```

```toml
config_version = 2

[agent]
connection = "deepseek-default"
model = "deepseek-v4-flash"

[connections.deepseek-default]
label = "DeepSeek"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = { source = "environment", name = "SIGIL_API_KEY" }

[connections.deepseek-default.options]
fim_model = "deepseek-v4-pro"
```

可复制文件见 [deepseek-basic.toml](../examples/config/deepseek-basic.toml)。

## 认证

示例只把当前连接绑定到 `SIGIL_API_KEY`。也可以在首次设置或 `/config` 中选择**受保护凭据存储**；此时 `sigil.toml` 只保存不透明的 `stored` ID。默认 `file` 使用 owner-only 的 `~/.sigil/credentials.json`，不会触发系统认证框。粘贴的密钥不是合法 connection 字段。

## 选项与可见限制

`base_url` 属于这条精确连接。`beta_base_url`、`anthropic_base_url`、`fim_model`、`strict_tools_mode` 和 `user_id_strategy` 放在 `[connections.deepseek-default.options]` 下，只作用于该 DeepSeek 路由。

[`deepseek-v4-flash-vision-exp`](https://api-docs.deepseek.com/guides/vision) 已作为实验模型内置，
也是 Sigil 唯一启用图片输入的 DeepSeek 精确模型 ID。选择该模型时，来自本机的 PNG、JPEG 和 WebP
附件会作为 OpenAI-compatible 图片内容块发送；其他 DeepSeek 模型 ID 仍会在请求发出前拒绝附件。
即使 provider 接受更多格式或远程图片 URL，Sigil 也不会自行抓取 URL 或扩大已声明的输入格式。

当前空闲会话可用 `/model deepseek-v4-flash-vision-exp` 切换。对于目录未列出的新发布或私有模型，
直接在 `/model` 后输入完整 ID；选择器会显示 **Use exact model ID** 候选，不会把它替换为名称相近的目录模型。

## 验证

运行 `sigil doctor`，确认 `default=deepseek-default/deepseek-v4-flash`、端点、凭据来源和就绪状态。

## 常见问题

- 认证失败：在启动 Sigil 的同一 Shell 中导出 `SIGIL_API_KEY`。
- 模型错误：检查 `[agent].connection` 与 `[agent].model` 组成的精确路由，以及任务角色覆盖设置。
- FIM 不可用：确认 `fim_model` 和端点都支持该能力。
- 流式响应较慢：检查网络和模型请求超时设置。

<!-- public-doc-cta: return-providers -->
下一步：[返回模型服务指南](providers.md)。
