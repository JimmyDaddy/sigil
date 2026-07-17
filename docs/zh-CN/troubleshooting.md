<!-- public-doc-role: troubleshooting; authority: symptom-to-action-authority; sections: decision-tree,quick-setup-opens-every-time,sigil-cannot-find-the-api-key,theme-colors-are-hard-to-read,the-wrong-workspace-is-being-used,a-file-tool-cannot-access-a-path,a-tool-needs-approval-in-headless-run,an-approval-was-denied-or-timed-out,mouse-or-clipboard-does-not-work,attention-notification-does-not-appear,session-restore-shows-interrupted-tools,context-usage-is-high,mcp-server-is-missing-failed-or-deferred,code-intelligence-is-not-ready,command-not-found-after-install,report-a-bug; cta: open-reference -->

# 排障

[文档首页](README.md) · [参考](reference.md) · [English](../en/troubleshooting.md)

先运行：

```bash
sigil doctor
```

需要为支持请求生成脱敏诊断文件时，使用 `sigil doctor --output json`。下面列出最常见的后续动作。

## 决策树

| 现象 | 先检查 | 下一步 |
|---|---|---|
| Setup 每次都打开 | Provider 凭据 | 重新打开 `/config` 并保存可用 provider |
| 文件范围不对 | Workspace 路径 | 从目标目录重新启动 |
| 工具被阻止 | 审批或 sandbox 提示 | 阅读原因；仅在确有需要时调整策略 |
| MCP 不可用 | `/config` → MCP Servers | 修正认证、配置或启动模式 |
| 终端输入异常 | 终端支持情况 | 换受支持的终端并运行 Doctor |
| 上下文接近上限 | 信息栏 | 完成当前步骤，或在可用时使用 `/compact` |

## Quick Setup 每次都打开

当前没有可用的 provider 配置或凭据。打开 `/config`，选择 provider，保存后运行 `/doctor`。精确字段见 [Providers](providers.md)和[配置字段参考](configuration-reference.md)。

## Sigil 找不到 API Key

确认凭据名称与所选 provider 匹配，并且启动 Sigil 的终端继承了它。修改 shell 变量后重启 Sigil。优先使用文档中的 provider 配置或系统凭据流程；不要把 secret 贴进 issue。

## 主题颜色难以阅读

打开 `/config` → **Appearance**，切换 theme 或 syntax style；也可以关闭信息栏。见[外观](appearance.md)。

## Workspace 不对

检查 Sigil 的启动目录和当前 `workspace.root`。从目标目录重新启动，或编辑当前使用的 `sigil.toml`；`workspace.root = "."` 会跟随启动目录。

外部目录默认不可访问，见[权限与 Sandbox](permissions-and-sandbox.md)。

## 文件工具不能访问某个路径

先读 tool card 中的错误。确认路径位于 workspace 内、没有被策略排除，也不是 symlink 逃逸。确需访问时只配置最窄的额外根目录，不要为了消除错误而扩大权限。

## Headless `run` 里工具需要审批

Headless 模式不能显示审批弹窗。为预期动作设置足够且最小的策略，或改为交互运行。不要让自动化拥有超出任务所需的权限。

## 审批被拒绝或超时

该动作没有执行。检查 preview，修正请求后重试。超时按拒绝处理，Sigil 不会静默继续。

## 鼠标或剪贴板不可用

在当前 `sigil.toml` 中检查 `[terminal].mouse_capture` 与 `osc52_clipboard`，修改后重启，再测试普通文本选择。`Ctrl-C` 复制选区；存在选区时，`Ctrl-L` 也复制该选区，没有选区时才复制最近一条助手回复。图片粘贴还依赖受支持的系统剪贴板。见[终端兼容性](terminal-compatibility.md)。

## Attention Notification 没有出现

通知默认关闭，而且依赖终端支持。在 `/config` → **Terminal** 中启用后运行 Doctor，并确认终端没有禁用 OSC 或 bell 通知。

## Session 恢复后显示 Interrupted Tools

进程停止时仍在运行的工作会标记为 interrupted，命令不会自动重放。检查 tool card，只在动作仍然需要时重试。

## Context Usage 很高

信息栏会显示 context pressure。开始大请求前，先完成或 checkpoint 当前工作。只有 Sigil 能为所选模型安全执行时 `/compact` 才可用；否则新建或 fork conversation。见[用户指南](user-guide.md#长上下文和压缩)。

## MCP Server 缺失、失败或 Deferred

打开 `/config` → **MCP Servers** 查看状态：

- **missing/failed：** 检查命令或 URL、认证和日志；
- **deferred：** 使用工具前先激活 server；
- **needs sign-in：** 打开 `/config` → **MCP Servers** → **Authentication**。

OAuth 只使用最新的登录 tab 或完整 callback URL。远端 revoke 失败时，本机凭据会保留并显示错误；你可以重试 revoke，或显式选择**只清除本机**，后者不表示远端 token 已撤销。Credential store、callback、refresh、目标拒绝和 `401` 的恢复步骤统一见 [MCP](mcp.md#oauth-认证)。

## Code Intelligence 未就绪

运行 Doctor，确认语言工具已安装，且能被启动 Sigil 的同一环境找到。语言服务不可用时 Sigil 可能以较少上下文继续，tool card 会提示该限制。

## 安装后 Command Not Found

打开新 shell，确认包管理器的 binary 目录位于 `PATH`。存在多个副本时，运行 `command -v sigil`（PowerShell 用 `Get-Command sigil`）并移除旧副本。重装命令只在[安装](installation.md)维护。

## 报告问题

运行 `sigil doctor --output json` 或使用 `/feedback`，检查导出文件后手动附加到相应 GitHub 表单。说明实际结果、预期结果、复现步骤、平台与终端，以及最小且安全的日志片段。删除项目内容和 secret；报告不会自动上传。

<!-- public-doc-cta: open-reference -->
下一步：[查找精确命令与键位](reference.md)。
