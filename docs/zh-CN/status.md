<!-- public-doc-role: status; authority: maturity-and-limit-authority; sections: supported-today,limited-or-advanced,not-supported-yet; cta: open-changelog -->

# 当前支持状态与未来工作

[文档首页](README.md) · [安装](installation.md) · [Changelog](changelog.md) · [English](../en/status.md)

Sigil 仍是 early preview。核心 TUI 工作流已经可用，但配置、插件、高级 sandbox 行为和自动化接口仍可能调整。Release 版本与安装命令统一在[安装](installation.md)和 [Changelog](changelog.md)维护。

## 当前支持

| 范围 | 当前支持 |
|---|---|
| Provider | DeepSeek、OpenAI-compatible Chat Completions、OpenAI Responses、Anthropic 与 Gemini；见 [Providers](providers.md) |
| 非交互入口 | Headless `run` 支持 text、JSON、JSONL；高级集成可使用带认证且仅监听本机的 `serve` |
| 平台 | macOS 与 Linux 是主要测试路径；Windows 使用 native PowerShell，并在 Doctor 中显示限制 |

## 有限制或高级用法

- Headless 模式不能发起交互审批，策略必须预先决定。
- 本地服务只监听本机，并要求 bearer 认证。
- Code intelligence 依赖启动环境中可用的语言工具。
- 外部目录默认不可访问；sandbox 强度随平台和后端而不同。
- Deferred MCP server 必须先激活，工具才可用。
- 图片输入受格式、来源、provider 与模型能力限制。
- 只有 Sigil 能为所选模型安全执行时，才会提供 context compaction。

## 暂不支持

当前不承诺自更新、稳定 plugin API、桌面应用、跨平台一致的 sandbox 保证，也不能在重启后继续当时仍在运行的子进程。

精确命令和键位见[参考](reference.md)，配置字段见[配置字段参考](configuration-reference.md)，问题处理见[排障](troubleshooting.md)。

<!-- public-doc-cta: open-changelog -->
下一步：[阅读 Changelog](changelog.md)。
