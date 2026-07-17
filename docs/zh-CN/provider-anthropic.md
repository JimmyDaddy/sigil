<!-- public-doc-role: provider-anthropic; authority: provider-specific-setup; sections: minimal-setup,authentication,options-and-visible-limits,verify,common-problems; cta: return-providers -->

# Anthropic Provider

[Provider 指南](providers.md) · [配置](configuration.md) · [English](../en/provider-anthropic.md)

## 最小设置

```bash
export SIGIL_ANTHROPIC_API_KEY="sk-ant-..."
sigil
```

```toml
[agent]
provider = "anthropic"
model = "claude-sonnet-4-5"

[providers.anthropic]
base_url = "https://api.anthropic.com"
anthropic_version = "2023-06-01"
max_tokens = 4096
```

可复制文件见 [anthropic.toml](../examples/config/anthropic.toml)。

## 认证

`SIGIL_ANTHROPIC_API_KEY` 优先于 `[providers.anthropic].api_key`。优先使用环境变量；保存的 key 是明文。

## 选项与可见限制

`SIGIL_ANTHROPIC_BASE_URL`、`SIGIL_ANTHROPIC_VERSION` 和 `SIGIL_ANTHROPIC_MAX_TOKENS` 覆盖对应配置字段。只有明确知道 Anthropic 功能需要时才使用 `beta_headers`。

图片只支持识别到的 Claude model ID 与接受的 dated variant。未知名称和 alias 会在发送前被拒绝。

## 验证

运行 `sigil doctor`，确认 provider、model、base URL、API version、token limit 和凭据来源。

## 常见问题

- Version/header 被拒绝：检查 `anthropic_version` 与 `beta_headers`。
- 输出提前结束：检查 `max_tokens` 和 model limit。
- 认证失败：检查环境变量或 config fallback。
- Tool 行为异常：确认所选 Claude model 支持 tool use。

<!-- public-doc-cta: return-providers -->
下一步：[返回 Provider 指南](providers.md)。
