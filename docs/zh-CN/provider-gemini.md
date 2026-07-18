<!-- public-doc-role: provider-gemini; authority: provider-specific-setup; sections: minimal-setup,authentication,options-and-visible-limits,verify,common-problems; cta: return-providers -->

# 接入 Gemini

[模型服务指南](providers.md) · [配置](configuration.md) · [English](../en/provider-gemini.md)

## 最小设置

```bash
export SIGIL_GEMINI_API_KEY="..."
sigil
```

```toml
[agent]
provider = "gemini"
model = "gemini-2.5-pro"

[providers.gemini]
base_url = "https://generativelanguage.googleapis.com/v1beta"
```

可复制文件见 [gemini.toml](../examples/config/gemini.toml)。

## 认证

`SIGIL_GEMINI_API_KEY` 优先于 `[providers.gemini].api_key`，并且不会改变其他 Google 工具使用的凭据。

## 选项与可见限制

`SIGIL_GEMINI_BASE_URL` 可以临时覆盖 `base_url`。模型可用性可能因账户和区域而异；请明确设置 `[agent].model`。

图片只支持已识别的 Gemini 模型 ID。浮动的 `latest` 名称、未知 ID 和别名会在发送前被拒绝。

## 验证

运行 `sigil doctor`，确认模型服务、具体模型、基础 URL 与凭据来源。

## 常见问题

- 认证失败：检查启动 Shell 中的 `SIGIL_GEMINI_API_KEY`。
- 找不到模型：确认模型名称、端点版本、账户和区域。
- 函数调用失败：确认模型与端点支持函数调用。
- 超时：检查网络和模型请求超时设置。

<!-- public-doc-cta: return-providers -->
下一步：[返回模型服务指南](providers.md)。
