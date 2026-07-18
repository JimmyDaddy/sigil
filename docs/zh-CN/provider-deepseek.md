<!-- public-doc-role: provider-deepseek; authority: provider-specific-setup; sections: minimal-setup,authentication,options-and-visible-limits,verify,common-problems; cta: return-providers -->

# 接入 DeepSeek

[模型服务指南](providers.md) · [配置](configuration.md) · [English](../en/provider-deepseek.md)

## 最小设置

```bash
export SIGIL_API_KEY="sk-..."
sigil
```

```toml
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[providers.deepseek]
base_url = "https://api.deepseek.com"
fim_model = "deepseek-v4-pro"
```

可复制文件见 [deepseek-basic.toml](../examples/config/deepseek-basic.toml)。

## 认证

`SIGIL_API_KEY` 优先于 `[providers.deepseek].api_key`。本机和 CI 中请优先使用环境变量；配置中保存的密钥是明文。

## 选项与可见限制

`base_url`、`beta_base_url`、`anthropic_base_url`、`fim_model`、`strict_tools_mode` 和 `user_id_strategy` 属于 DeepSeek 专项选项。环境覆盖分别使用 `SIGIL_BASE_URL`、`SIGIL_BETA_BASE_URL`、`SIGIL_ANTHROPIC_BASE_URL`、`SIGIL_FIM_MODEL`、`SIGIL_STRICT_TOOLS_MODE` 与 `SIGIL_USER_ID_STRATEGY`。

DeepSeek 图片输入尚未启用。附加图片会在发送请求前被拒绝；需要处理图片时，请选择支持图片的模型服务。

## 验证

运行 `sigil doctor`，确认模型服务、具体模型、基础 URL 和凭据来源。

## 常见问题

- 认证失败：在启动 Sigil 的同一 Shell 中导出 `SIGIL_API_KEY`。
- 模型错误：检查 `[agent].model` 和任务角色覆盖设置。
- FIM 不可用：确认 `fim_model` 和端点都支持该能力。
- 流式响应较慢：检查网络和模型请求超时设置。

<!-- public-doc-cta: return-providers -->
下一步：[返回模型服务指南](providers.md)。
