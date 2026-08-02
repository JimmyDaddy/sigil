<!-- public-doc-role: status; authority: maturity-and-limit-authority; sections: supported-today,limited-or-advanced,not-supported-yet; cta: open-changelog -->

# 当前支持状态与后续计划

[文档首页](README.md) · [安装](installation.md) · [变更记录](changelog.md) · [English](../en/status.md)

Sigil 仍处于早期预览阶段。核心 Desktop 与 TUI 工作流已经可用，但配置、插件、高级沙箱行为和自动化接口仍可能调整。发布版本与安装命令统一在[安装](installation.md)和[变更记录](changelog.md)中维护。

## 当前支持

| 范围 | 当前支持 |
|---|---|
| 模型服务 | DeepSeek、OpenAI-compatible Chat Completions、OpenAI Responses、Anthropic 与 Gemini；见[模型服务指南](providers.md) |
| 非交互入口 | `run` 支持纯文本、JSON 和 JSONL；高级集成可以使用带认证且仅监听本机的 `serve` |
| 平台 | 公开 Desktop beta 提供 Apple 芯片与 Intel 的签名、公证 DMG。TUI 主要测试 macOS 与 Linux；Windows 使用原生 PowerShell，并在 Doctor 中显示限制 |

## 有限制或高级用法

- 非交互模式无法发起人工审批，相关策略必须提前配置。
- 本地服务只监听本机，并要求 Bearer 令牌认证。
- 代码智能依赖启动环境中可用的语言工具。
- 外部目录默认不可访问；沙箱强度会因平台和执行后端而异。
- 延迟启动的 MCP 服务必须先激活，工具才可用。
- 图片输入受格式、来源、模型服务和具体模型能力限制。
- 只有能够为所选模型安全精简上下文时，Sigil 才会提供相应操作。
- Desktop 设置页与 TUI/CLI 可以检查当前发布渠道。安装更新必须由用户明确触发，会独立验证签名/checksum 发布信息；npm、Homebrew、Cargo 或源码管理的安装会交还给原安装器。
- Desktop 与 TUI 是复用同一 runtime 语义的一等产品表面。macOS beta 已成为公开的签名安装渠道，提供 Apple 芯片与 Intel DMG；TUI beta 通过 npm、Homebrew、源码 tag 与发布压缩包分发。Desktop 提供有边界的已保存对话、工作区服务保持打开时的运行重连与控制，以及独立的工具、差异、审批和验证界面。紧凑导航把工作区选择收入顶栏，每个对话行可直接打开，只有存在证据时才显示验证面板。**Appearance** 菜单（`Cmd/Ctrl+,`）可跟随系统，也可持久保存应用级亮色或暗色选择，不会中断当前对话。

## 暂不支持

目前尚不提供无人值守的后台更新安装、自动重启或稳定插件 API，也不承诺跨平台一致的沙箱能力。Desktop beta 会在 DMG 旁发布分架构的签名更新包，但是否安装和何时重启仍由用户决定。桌面应用或其工作区服务重启后，同样无法继续之前仍在运行的子进程。

精确命令和键位见[参考](reference.md)，配置字段见[配置字段参考](configuration-reference.md)，问题处理见[排障](troubleshooting.md)。

<!-- public-doc-cta: open-changelog -->
下一步：[阅读变更记录](changelog.md)。
