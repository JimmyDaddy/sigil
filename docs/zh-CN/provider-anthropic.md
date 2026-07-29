<!-- public-doc-role: provider-anthropic; authority: provider-specific-setup; sections: minimal-setup,authentication,options-and-visible-limits,verify,common-problems; cta: return-providers -->

# 接入 Anthropic

[模型服务指南](providers.md) · [配置](configuration.md) · [English](../en/provider-anthropic.md)

## 最小设置

```bash
export SIGIL_ANTHROPIC_API_KEY="sk-ant-..."
sigil
```

```toml
config_version = 2

[agent]
connection = "anthropic-default"
model = "claude-sonnet-4-5"

[connections.anthropic-default]
label = "Anthropic"
provider = "anthropic"
protocol = "anthropic_messages"
base_url = "https://api.anthropic.com"
credential = { source = "environment", name = "SIGIL_ANTHROPIC_API_KEY" }

[connections.anthropic-default.options]
anthropic_version = "2023-06-01"
max_tokens = 4096
```

可复制文件见 [anthropic.toml](../examples/config/anthropic.toml)。

## 认证

示例把当前连接绑定到 `SIGIL_ANTHROPIC_API_KEY`。首次设置和 `/config` 也可以把密钥保存到受保护凭据存储；`sigil.toml` 中只保留不透明的 `stored` ID。默认 `file` 使用 owner-only credential file，不会触发系统认证框。

## 选项与可见限制

`anthropic_version`、`max_tokens` 与 `beta_headers` 是 `[connections.anthropic-default.options]` 下的 provider 专项字段。只有明确知道 Anthropic 功能需要时才使用 `beta_headers`。

图片只支持已识别的 Claude 模型 ID 和允许的带日期版本。未知名称和别名会在发送前被拒绝。

## 验证

运行 `sigil doctor`，确认 `default=anthropic-default/claude-sonnet-4-5`、端点、凭据来源和就绪状态。

## 常见问题

- 版本或请求头被拒绝：检查 `anthropic_version` 与 `beta_headers`。
- 输出提前结束：检查 `max_tokens` 和模型上限。
- 认证失败：检查绑定的环境变量，或在 `/config` 中修复当前连接。
- 工具行为异常：确认所选 Claude 模型支持工具调用。

<!-- public-doc-cta: return-providers -->
下一步：[返回模型服务指南](providers.md)。
