# Sigil 用户文档

[English](../en/README.md)

Sigil 是一个 TUI-first coding agent。本页只提供精简的用户文档路径地图，不重复 release 版本、安装命令或 provider 凭据。需要当前细节时，请进入对应主题的权威页面。

这些文档跟随 `main`，已打包发布的 alpha 是 `v0.0.1-alpha.3`。请通过 [Unreleased](changelog.md#unreleased-main) 确认该版本之后新增的能力。

## 从这里开始

第一次使用 Sigil 时，建议按顺序阅读：

1. [快速上手](quickstart.md)：使用推荐的 npm alpha 路径，启动 TUI，完成 Quick Setup，并跑完第一次有用的会话。
2. [安装](installation.md)：选择安装渠道，或查看权威的更新、卸载和 release archive 说明。
3. [视觉导览](visual-tour.md)：查看主会话、审批、配置、verification、checkpoint recovery 和 compaction preview 界面。
4. [TUI 使用指南](user-guide.md)：学习界面布局、操作方式、图片输入、attention notification、会话、审批、计划任务、verification、recovery、context controls 和 code intelligence。
5. [安全与权限](safety.md)：理解什么能运行、什么需要审批，以及如何检查高风险动作。
6. [排障](troubleshooting.md)：诊断 setup、认证、终端、MCP、code intelligence 和恢复问题。

## 按任务选择

| 我想要... | 阅读 |
| --- | --- |
| 第一次试用 Sigil | [快速上手](quickstart.md) |
| 安装、更新或卸载 Sigil | [安装](installation.md) |
| 了解产品界面 | [视觉导览](visual-tour.md) |
| 学习 TUI 和会话工作流 | [TUI 使用指南](user-guide.md) |
| 参考真实 coding task 的流程 | [常见工作流](workflows.md) |
| 复用可复制的 prompt 模式 | [Cookbook](cookbook.md) |
| 配置共享的 workspace、权限、任务、终端或工具行为 | [配置](configuration.md) |
| 选择 provider 或配置 provider 认证 | [Provider 指南](providers.md) |
| 理解审批、workspace 边界和 MCP trust | [安全与权限](safety.md) |
| 理解隐私、provider context、session log 和 secret | [隐私与数据处理](privacy.md) |
| 通过 MCP 增加外部工具 | [MCP 接入指南](mcp.md) |
| 查询命令、键位、路径和恢复事实 | [参考](reference.md) |
| 验证 attention notification、图片剪贴板、mouse capture、OSC52、tmux、SSH 或 WSL 行为 | [Terminal 兼容性](terminal-compatibility.md) |
| 排查问题或报告 bug | [排障](troubleshooting.md) |
| 查看当前支持承诺 | [当前支持状态与未来工作](status.md) |
| 阅读用户可见 release notes | [用户 Changelog](changelog.md) |

## 维护者文档

以上页面说明用户可见的产品行为。架构、实现和协作约束位于仓库中的 [`dev/docs`](https://github.com/JimmyDaddy/sigil/tree/main/dev/docs)、[`dev/governance`](https://github.com/JimmyDaddy/sigil/tree/main/dev/governance) 和 [`AGENTS.md`](https://github.com/JimmyDaddy/sigil/blob/main/AGENTS.md)。
