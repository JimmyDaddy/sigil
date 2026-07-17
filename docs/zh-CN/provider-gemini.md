<!-- public-doc-role: provider-gemini; authority: provider-specific-setup; sections: minimal-setup,authentication,options-and-visible-limits,verify,common-problems; cta: return-providers -->

# Gemini Provider

[Provider 指南](providers.md) · [配置](configuration.md) · [English](../en/provider-gemini.md)

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

`SIGIL_GEMINI_BASE_URL` 临时覆盖 `base_url`。Model 可用性可能因 account 和 region 不同；请明确设置 `[agent].model`。

图片只支持识别到的 Gemini model ID。Floating `latest` 名称、未知 ID 和 alias 会在发送前被拒绝。

## 验证

运行 `sigil doctor`，确认 provider、model、base URL 与凭据来源。

## 常见问题

- 认证失败：检查启动 shell 中的 `SIGIL_GEMINI_API_KEY`。
- Model not found：确认 model name、endpoint version、account 和 region。
- Function call 失败：确认 model 与 endpoint 支持 function calling。
- Timeout：检查网络和 model-request timeout。

<!-- public-doc-cta: return-providers -->
下一步：[返回 Provider 指南](providers.md)。
