<!-- public-doc-role: status; authority: maturity-and-limit-authority; sections: supported-today,limited-or-advanced,not-supported-yet; cta: open-changelog -->

# 当前支持状态与后续计划

[文档首页](README.md) · [安装](installation.md) · [变更记录](changelog.md) · [English](../en/status.md)

Sigil 仍处于早期预览阶段。核心 TUI 工作流已经可用，但配置、插件、高级沙箱行为和自动化接口仍可能调整。发布版本与安装命令统一在[安装](installation.md)和[变更记录](changelog.md)中维护。

## 当前支持

| 范围 | 当前支持 |
|---|---|
| 模型服务 | DeepSeek、OpenAI-compatible Chat Completions、OpenAI Responses、Anthropic 与 Gemini；见[模型服务指南](providers.md) |
| 非交互入口 | `run` 支持纯文本、JSON 和 JSONL；高级集成可以使用带认证且仅监听本机的 `serve` |
| 平台 | macOS 与 Linux 是主要测试平台；Windows 使用原生 PowerShell，已知限制会显示在 Doctor 中 |

## 有限制或高级用法

- 非交互模式无法发起人工审批，相关策略必须提前配置。
- 本地服务只监听本机，并要求 Bearer 令牌认证。
- 代码智能依赖启动环境中可用的语言工具。
- 外部目录默认不可访问；沙箱强度会因平台和执行后端而异。
- 延迟启动的 MCP 服务必须先激活，工具才可用。
- 图片输入受格式、来源、模型服务和具体模型能力限制。
- 只有能够为所选模型安全精简上下文时，Sigil 才会提供相应操作。
- 贡献者可以从 `main` 构建可选的桌面壳进行 dogfood。它复用 TUI 的本机服务、审批、会话和验证契约，但目前不是受支持的安装渠道。

## 暂不支持

目前尚不提供自动更新、稳定的插件 API、已签名或公证的桌面安装包以及桌面更新渠道，也不承诺跨平台一致的沙箱能力；重启后同样无法继续之前仍在运行的子进程。

精确命令和键位见[参考](reference.md)，配置字段见[配置字段参考](configuration-reference.md)，问题处理见[排障](troubleshooting.md)。

<!-- public-doc-cta: open-changelog -->
下一步：[阅读变更记录](changelog.md)。
