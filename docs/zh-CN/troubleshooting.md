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
| 每次都进入快速设置 | 模型服务凭据 | 重新打开 `/config`，选择可用的模型服务并保存 |
| 文件范围不对 | 工作区路径 | 从目标目录重新启动 |
| 工具被阻止 | 审批或沙箱提示 | 阅读原因；仅在确有需要时调整策略 |
| MCP 不可用 | `/config` → MCP Servers | 修正认证、配置或启动模式 |
| 终端输入异常 | 终端支持情况 | 换用受支持的终端并运行 Doctor |
| 上下文接近上限 | 信息栏 | 完成当前步骤，或在可用时使用 `/compact` |

## 每次都会进入快速设置

当前没有可用的模型服务配置或凭据。打开 `/config`，选择模型服务，保存后运行 `/doctor`。精确字段见[模型服务指南](providers.md)和[配置字段参考](configuration-reference.md)。

## Sigil 找不到 API 密钥

确认凭据名称与所选模型服务匹配，并且启动 Sigil 的终端能够读取它。修改 Shell 环境变量后请重启 Sigil。优先使用文档中的环境变量或系统凭据流程；不要把密钥粘贴到问题单中。

## 主题颜色难以阅读

打开 `/config` → **Appearance**，切换主题或语法高亮样式；也可以关闭信息栏。见[外观](appearance.md)。

## 工作区不对

检查 Sigil 的启动目录和当前 `workspace.root`。可以从目标目录重新启动，也可以编辑正在使用的 `sigil.toml`；`workspace.root = "."` 表示使用启动目录作为工作区。

外部目录默认不可访问，见[权限与沙箱](permissions-and-sandbox.md)。

## 文件工具不能访问某个路径

先阅读工具卡片中的错误。确认路径位于工作区内、没有被策略排除，也没有通过符号链接指向工作区外。确实需要访问时，只配置范围最小的额外根目录，不要仅仅为了消除错误而扩大权限。

## 非交互 `run` 中的工具需要审批

非交互模式不能显示审批弹窗。请为预期操作预先配置足够但尽可能小的权限，或者改用交互模式。不要让自动化任务拥有超出实际需要的权限。

## 审批被拒绝或超时

该操作没有执行。检查预览，修正请求后再重试。审批超时会按拒绝处理，Sigil 不会在后台继续执行。

## 鼠标或剪贴板不可用

在当前 `sigil.toml` 中检查 `[terminal].mouse_capture` 与 `osc52_clipboard`，修改后重启，再测试普通文本选择。`Ctrl-C` 复制选区；存在选区时，`Ctrl-L` 也复制该选区，没有选区时才复制最近一条助手回复。图片粘贴还依赖受支持的系统剪贴板。见[终端兼容性](terminal-compatibility.md)。

## 失焦通知没有出现

通知默认关闭，而且依赖终端支持。在 `/config` → **Terminal** 中启用后运行 Doctor，并确认终端没有禁用 OSC 或响铃通知。

## 恢复会话后显示工具已中断

进程停止时仍在运行的工具会标记为“已中断”，命令不会自动重放。请检查工具卡片，只有确认该操作仍然需要时才重试。

## 上下文用量很高

信息栏会显示上下文压力。开始大型任务前，先完成当前步骤或创建检查点。只有能够为所选模型安全精简上下文时，Sigil 才会启用 `/compact`；否则请新建会话，或从当前会话分叉。见[用户指南](user-guide.md#长上下文和压缩)。

## MCP 服务缺失、失败或尚未激活

打开 `/config` → **MCP Servers** 查看状态：

- **missing/failed：** 检查命令或 URL、认证信息和日志；
- **deferred：** 使用工具前先激活服务；
- **needs sign-in：** 打开 `/config` → **MCP Servers** → **Authentication**。

OAuth 只使用最新打开的登录标签页或完整的回调 URL。远端撤销失败时，本机凭据会保留并显示错误；你可以重试撤销，也可以明确选择**只清除本机**。后者并不表示远端令牌已经撤销。凭据存储、回调、刷新、目标拒绝和 `401` 的恢复步骤统一见 [MCP 指南](mcp.md#oauth-认证)。

## 代码智能未就绪

运行 Doctor，确认语言工具已经安装，而且启动 Sigil 的同一环境可以找到它。语言服务不可用时，Sigil 可能在缺少部分代码上下文的情况下继续工作，工具卡片会明确提示这一限制。

## 安装后找不到命令

打开新的 Shell，确认包管理器的可执行文件目录已经加入 `PATH`。存在多个副本时，运行 `command -v sigil`（PowerShell 使用 `Get-Command sigil`）并移除旧副本。重装命令统一在[安装](installation.md)维护。

## 报告问题

运行 `sigil doctor --output json` 或使用 `/feedback`。先检查导出文件，再手动附加到相应的 GitHub 表单。请说明实际结果、预期结果、复现步骤、平台与终端，并只提供足够定位问题的安全日志片段。删除项目内容和密钥；报告不会自动上传。

<!-- public-doc-cta: open-reference -->
下一步：[查找精确命令与键位](reference.md)。
