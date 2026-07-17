# 排障

[文档首页](README.md) · [English](../en/troubleshooting.md)

当 setup、认证、MCP、code intelligence 或终端行为异常时，先运行内置诊断：

```bash
sigil doctor
```

在 TUI 内：

```text
/doctor
```

Report 会展示状态摘要、warning/error 和 remediation line。它会说明 credential 来源，但不会打印 secret 值。

如果需要更适合附加到私密或公开支持请求的结构化报告，运行：

```bash
sigil doctor --output json
```

两种形式都只在本机生成，不会发起 provider 请求。

## 决策树

知道症状时从这里开始：

| 症状 | 先检查 | 然后阅读 |
| --- | --- | --- |
| Sigil 每次都打开 Quick Setup | `sigil doctor` 中的 config resolution 和 load errors | [Quick Setup 每次都打开](#quick-setup-每次都打开) |
| Provider 认证失败 | `sigil doctor` 中的 API key source | [Sigil 找不到 API Key](#sigil-找不到-api-key) |
| Sigil 读取或编辑了错误仓库 | `/doctor` 中的 workspace path | [Workspace 不对](#workspace-不对) |
| 某个文件路径被拒绝 | Workspace confinement 和 symlink target | [文件工具不能访问某个路径](#文件工具不能访问某个路径) |
| `sigil run` 显示需要审批 | Headless mode 不能展示 approval card | [Headless run 里工具需要审批](#headless-run-里工具需要审批) |
| Approval 消失或被拒绝 | Timeout 或 deny action | [审批被拒绝或超时](#审批被拒绝或超时) |
| 鼠标或复制不可用 | `/config` 和 `/doctor` 中的 Terminal section | [鼠标或剪贴板不可用](#鼠标或剪贴板不可用) |
| 主题颜色难以阅读 | `sigil doctor` 或 `/doctor` 中的 appearance warning | [主题颜色难以阅读](#主题颜色难以阅读) |
| 恢复 session 后显示 interrupted tools | Recovery 投影了未完成工具 | [Session 恢复后显示 Interrupted Tools](#session-恢复后显示-interrupted-tools) |
| MCP tools 缺失 | Server startup mode 和 lifecycle state | [MCP Server 缺失、失败或 Deferred](#mcp-server-缺失失败或-deferred) |
| LSP tools 不可用 | Code-intelligence readiness rows | [Code Intelligence 未就绪](#code-intelligence-未就绪) |

## Quick Setup 每次都打开

常见原因：

- 当前配置解析路径下没有 config 文件。
- config 文件存在但加载失败。
- workspace 或 provider 字段不完整。

检查：

```bash
sigil doctor
```

如果使用非默认配置路径，对 doctor 也传同一个路径：

```bash
sigil --config ./sigil.toml doctor
```

## Sigil 找不到 API Key

1. 打开 [provider 认证映射](providers.md#认证优先级)，找到 `[agent].provider` 当前配置的 provider。
2. 在启动 `sigil` 的同一个 shell 中，执行对应 provider 专页提供的可复制环境变量命令。
3. 再次运行 `sigil doctor`，确认 provider 和 key 来源都符合预期。

Sigil 会主动忽略可能与其他工具共享状态的通用凭据变量。各 provider 专页是认证环境变量和 config fallback 的权威来源。

如果通过 `/config` 保存 key，它会以明文写入 `sigil.toml`。私有本地配置可以这样做，但不要提交。

## 主题颜色难以阅读

运行 `sigil doctor` 或 `/doctor` 并检查 `appearance:*` warning。这些检查覆盖用户可见的文字/背景对比、语义颜色区分度，以及边框和相邻背景之间的结构提示。

移除或编辑 warning 中列出的 `[appearance.colors]` 项，让对应 token 组合有更强对比或更清晰区分。只有在没有覆盖项、或现有覆盖项与新内置主题兼容时，才适合通过 `/config` 切换主题来修复。

## Workspace 不对

常规配置：

```toml
[workspace]
root = "."
```

`.` 会解析成启动 `sigil` 时所在目录，而不是配置文件所在目录。

修复：

```bash
cd /path/to/the/repository
sigil
```

运行 `/doctor` 并检查 report 中的 workspace path。

## 文件工具不能访问某个路径

Sigil 会把文件工具限制在 workspace root 内，并拒绝：

- workspace 外的绝对路径；
- 使用 `..` 逃逸 workspace 的路径；
- 解析后指向 workspace 外的 symlink。

如果确实需要 external directory access，配置 `[permission.external_directory]`，并保持默认模式保守。

## Headless `run` 里工具需要审批

交互式 TUI session 可以展示 approval modal。Headless `sigil run` 不能交互询问，所以 `ask` 决策会作为结构化 `approval_required` tool error 返回给模型。

自动化场景中，要么保持任务只读，要么只为你信任的窄动作定义明确 permission rules。

## 审批被拒绝或超时

如果长时间没有决策，Sigil 会 deny request，避免 worker 一直等待。

处理方式：

1. 阅读被拒绝的 tool summary。
2. 用更窄范围重新描述任务。
3. 如果 diff 太大，要求 Sigil 先提出方案。

## 鼠标或剪贴板不可用

打开 `/config`，查看 `Terminal` section。

常见缓解配置：

```toml
[terminal]
keyboard_enhancement = "off"
mouse_capture = false
osc52_clipboard = false
scroll_sensitivity = 3
```

`keyboard_enhancement` 在下次启动时解析。`mouse_capture` 下次启动生效。`osc52_clipboard` 每次复制时检查。`scroll_sensitivity` 在保存配置并重新加载后生效。

tmux、screen、SSH、WSL 和手工 smoke check 见 [Terminal 兼容性检查清单](terminal-compatibility.md)。

## Attention Notification 没有出现

打开 `/config` → `Terminal`，确认 notification 已开启。临时把长任务阈值改为 `1000` ms，并选择 `bell`，先把 terminal protocol 支持与通知时机分开验证。部分终端会静音 BEL；已知兼容的 profile 可尝试 `osc9` 或 `osc777`。在 tmux 或 screen 下，还要确认 escape-sequence pass-through。只有 terminal 已提供可靠 focus event 时，聚焦的 Sigil 才会抑制通知。

运行 `/doctor` 确认已保存的开关、method 和 threshold。通知失败不会使 active run 失败；短回复、取消、progress 或恢复历史 session 也不会发送通知。

## Session 恢复后显示 Interrupted Tools

这是预期行为。进程退出、崩溃、终端关闭或 cancellation 发生在工具运行中时，Sigil 会把 started-but-unfinished tools 恢复为 interrupted results，不会静默重放。

用 `/resume` 选择 session。如果计划任务仍未完成，可以在 composer 里输入 guidance，或运行：

```text
/task continue
```

## Context Usage 很高

Info rail 会显示 provider 返回的上一轮 prompt usage。如果 `ctx` 行提示窗口不可用，可以设置 `fallback_context_window_tokens`。soft 和 hard threshold 表示上下文压力。一次成功 turn 到达 hard threshold 并完全进入 idle 后，Sigil 可以在后台准备一次通过本地验证的 compaction。queued request 的 exact frozen material 超过已准入 budget 时，也可以在 promotion 前先 compact。pinned 官方 OpenAI Responses profile 可在精确、无 output 的 context rejection 与两次受审计计数成功后执行一次受控 overflow recovery。

手动 compaction：

```text
/compact
```

该命令会打开只读 V2 折叠预览。打开预览不会追加 compaction record，也不会改写 session history。如果审查显示 target ready，按 `Enter` 可确认一次手动应用；否则审查会说明缺少哪项本地 admission 前提。idle 与 queued pre-turn 路径可以使用同一 exact local admission。受控 OpenAI Responses overflow recovery 使用独立审计的远端计数证据，不由此 review 发起。

可以安装本地手动 admission 所需、经过 checksum 验证的 DeepSeek V4 Flash tokenizer：

```text
sigil tokenizer install deepseek-v4-flash
```

该命令会在下载 public artifact 前打印 network disclosure。安装 tokenizer 不会应用 compaction，也不会改变 session。

## MCP Server 缺失、失败或 Deferred

检查：

- `command` 是否在 `PATH` 上可用？
- `args` 中的路径是否为绝对路径且存在？
- 测试期间是否应该先设 `required = false`？
- `startup = "lazy"` 是否符合预期？Lazy server 激活前不会注册工具。
- `pin_version = true` 时，pinned identity 是否匹配 observed server identity？

运行：

```text
/doctor
```

在 TUI 中，失败的 eager MCP server 不应该阻塞普通 chat 或使用内置工具的 planned task。

OAuth 请根据 `/config` → **MCP Servers** → **Authentication** 中的 typed state 处理：

| 状态或错误 | 处理方式 |
| --- | --- |
| `authentication required` | 明确选择**登录**；eager 启动不会自动打开浏览器。 |
| metadata unavailable / destination rejected | 检查 HTTPS、resource URL、Network disclosure、proxy/domain policy 与 authorization server metadata。 |
| callback invalid | 重新开始登录，只使用最新浏览器 tab。手动返回时粘贴包含原始 state 的完整 callback URL。 |
| credential store unavailable/rejected | 解锁或启用平台原生 Keychain、Credential Manager 或 Secret Service。Sigil 没有明文凭据 fallback。 |
| refresh rejected | 重新登录。Sigil 会停用无效 refresh credential，不会循环重试。 |
| 远端 revoke 失败 | 本机凭据会保留。重试远端退出，或检查提示后明确清除。 |
| budget exhausted | 增加有边界的 Web budget，或等待其他 Web 工作结束，再重试显式动作。 |

OAuth redirect 与自动请求 retry 均关闭。请求发送后收到 `401` 时，Sigil 不会重放该请求。完整 callback 与凭据契约见 [MCP OAuth 认证](mcp.md#oauth-认证)。

## Code Intelligence 未就绪

检查：

- `[code_intelligence].enabled`
- 对应 language server 是否已安装并在 `PATH` 上；
- discovery 是否启用；
- server 保持默认 `trust_required = true` 时，当前精确 workspace 是否已被信任；
- `/config` 里的 LSP readiness rows；
- `/doctor` 输出。

全新的 headless `sigil run` trust 状态是 `Unknown`，也不会复用另一个 session 的 decision，因此要求 trust 的 LSP 会按设计保持停止。需要 session-bound trusted LSP 时应使用 TUI；只有该 headless 进程启动策略确实合适时，才应显式设置 `trust_required = false`。如果没有 LSP server，Rust 项目仍可使用 Tree-sitter fallback 提供 outline 和 syntax diagnostics。普通 chat 和文件工具不受影响。

## 安装后 Command Not Found

确认安装器已经完成，然后检查当前 shell 的 `PATH`：

```bash
echo "$PATH"
```

在[安装](installation.md)中找到原安装渠道，确认该渠道的 binary 位置，并重新执行对应的安装或更新命令。安装器专项命令只在安装页维护，避免这里出现过期副本。

## 报告问题

如果决策树和 `sigil doctor` 仍无法解决问题，在 TUI 中运行 `/feedback`。首先出现的是隐私预览：此时没有写入或上传任何内容。预览会列出将包含的诊断类别，以及明确排除的私密类别。

只有确实要在 Sigil 本地 cache 中保存一份 JSON 报告时才按 `Enter`；Sigil 绝不会自动上传。导出后再次按 `Enter` 可在 TUI 内检查实际写盘的脱敏 JSON，按 `O` 在文件管理器中定位，或按 `B` 打开[结构化 Bug 表单](https://github.com/JimmyDaddy/sigil/issues/new?template=bug-report.yml)。`C` 复制报告路径，`U` 复制表单链接；附加和提交报告仍由你完成。

报告可能包含 build、操作系统与架构信息、脱敏 doctor 状态、provider 和 model 标签、MCP alias，以及 capability 或 sandbox 状态。它会排除对话文本、tool input/output、文件内容和 diff、配置文件正文、credential 与环境变量名称及值、私有 endpoint、本地路径和 session log 内容。

疑似安全漏洞不要提交公开 Issue；请改为按照仓库的[安全策略](https://github.com/JimmyDaddy/sigil/blob/main/SECURITY.md)私下报告。

## 提 Issue 时提供什么

建议包括：

- `sigil --version`
- 已自行检查的 `/feedback` JSON 报告；TUI 不可用时可改用 `sigil doctor --output json`
- 操作系统和终端模拟器
- 是否在 tmux、screen、SSH 或 WSL 中
- 能复现问题的最小 prompt 或 command

除非确有必要并且你已自行检查和移除私密内容，否则不要附加配置文件、session log、源码文件或原始终端 transcript。
