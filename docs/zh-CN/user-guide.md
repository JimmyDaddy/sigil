# Sigil TUI 使用指南

[English](../en/user-guide.md)

本文面向 Sigil 的日常使用者，重点说明 TUI 里能看到和能操作的内容。开发约束、crate 结构和测试规则请看 `dev/governance/*`。

## 启动

启动 TUI：

```bash
cargo run -p sigil-tui
```

如果没有可用配置，Sigil 会进入 Quick Setup。首配流程只要求确认当前工作区、选择模型并填写认证。完成后会写入 `workspace.root = "."`，表示启动 TUI 时所在目录就是当前工作区。

认证方式和环境变量配置见 [configuration.md](configuration.md)。

## 主界面

Sigil 的主界面围绕这几个区域组织：

- Chat / transcript：展示用户消息、assistant 回复、thinking 摘要和工具活动。
- Composer：底部输入区，默认保持可见，发送后会清空并继续可输入。
- Info rail：右侧状态栏，展示 session、权限、模型、LSP、usage 和 controls。
- Activity：工具调用结果，例如读文件、搜索、执行命令、文件修改和 code diagnostics。
- Approval modal：工具需要确认时出现的审批卡片，展示 summary、files、diff 和 actions。

主路径是直接在 composer 输入任务。不要把 Sigil 当成命令集合使用；slash command 只处理少数高频控制动作。

## 高频操作

| 操作 | 快捷键 |
| --- | --- |
| 打开帮助 | `F1` |
| 打开 slash command selector | `/` |
| 回看 transcript | `PageUp/PageDown`、`Ctrl-U/D`、`Ctrl-Home/End` |
| 切换默认权限模式 | `Shift-Tab` |
| 取消当前运行 | `Ctrl-C` |
| 退出当前浮层或清除 activity focus | `Esc` |
| 聚焦最近 activity | `Ctrl-G` |
| 切换 activity | `Alt-J` / `Alt-K` |
| 展开或收起 thinking / activity | `Ctrl-T` |

Composer 聚焦时，`Up/Down` 会优先处理输入历史或多行输入里的光标移动。

## Slash Commands

| 命令 | 用途 |
| --- | --- |
| `/config` | 打开 TUI 配置页 |
| `/doctor` | 运行本地环境诊断 |
| `/resume` | 选择并恢复历史 session |
| `/model <flash|pro|id>` | 切换下一轮使用的模型，并开启 fresh session |
| `/effort <low|medium|high|max>` | 切换下一轮 reasoning effort |
| `/compact` | 手动压缩当前会话的 provider 可见上下文 |
| `/quit` | 退出 TUI |

`/model`、`/effort` 和 `/resume` 会显示候选项。可以用 `Up/Down` 选中，`Tab` 接受，`Enter` 执行。

## 审批和文件变更

读文件和搜索这类只读工具通常可以直接执行。写文件、编辑文件、删除文件、shell 执行和外部 MCP 工具会按权限策略进入审批或拒绝。

审批卡片里重点看：

- Summary：这次工具调用要做什么。
- Files：可能影响哪些文件。
- Diff：写操作的变更预览。
- Actions：选择 allow 或 deny。

审批支持 `Left/Right` 选择动作后 `Enter` 确认，也保留 `Y/N` 快捷确认。长时间不决策会自动 deny，避免后台 worker 一直等待。

文件变更工具执行后，activity 会保留 bounded diff。大 diff 会截断并提示隐藏行数。

## Session 和恢复

默认 session log 写入当前工作区：

```text
.sigil/sessions/
```

Sigil 使用 append-only JSONL 保存 session 和控制状态。对使用者来说，这意味着：

- 重启 TUI 后默认恢复最近一次 session。
- 取消运行后，已经写入的消息和工具结果不会因为内存状态丢失而消失。
- 已开始但未完成的工具执行会在恢复后显示为 interrupted。
- 文件变更 activity 会随 session restore 恢复，仍可回看当时捕获的 diff 摘要。
- `/config` 保存新的默认 provider/model 不会改写当前 session identity；新默认值用于后续新 session。

## 长上下文和压缩

Info rail 会显示当前 context 使用状态。Sigil 会按模型窗口或配置的 fallback window 计算 soft / hard threshold：

- soft threshold：提示上下文压力变高。
- hard threshold：当前 run 回到 idle 后自动压缩，不抢占正在流式执行的任务。
- `/compact`：手动压缩当前 session 的 provider 可见上下文。

压缩只追加控制记录，不改写旧历史。

## Code Intelligence

Code intelligence 默认关闭。开启后，Sigil 会注册只读代码工具：

- `code_symbols`
- `code_workspace_symbols`
- `code_definition`
- `code_references`
- `code_diagnostics`

TUI 里可以用 `Alt-D` 对当前 git changed source files 触发 diagnostics 检查。结果会作为普通 activity 展示，并在 Info rail 的 LSP 区保留摘要。

没有可用 LSP server 时，Rust 项目会尽量回退到 Tree-sitter Rust outline / syntax diagnostics。失败不会阻塞普通 chat 和工具调用。

配置方式见 [configuration.md](configuration.md)。

## 常见问题

### 启动后直接进入 Quick Setup

说明没有找到可用配置，或者配置加载失败。完成 Quick Setup 后再进入主界面。

### API key 要不要写入配置文件

推荐临时或 CI 场景使用 `SIGIL_API_KEY`。如果通过 Quick Setup 或 `/config` 写入本地配置，`api_key` 会以 plaintext 保存；`doctor` 会把这个状态作为 warning 并给出修复建议。不要提交真实 `sigil.toml`。

### 为什么 CLI 很少

Sigil 的普通用户入口是 TUI。CLI 当前主要用于自动化、脚本和调试，不承载完整产品心智。

### 为什么有些工具需要审批

Sigil 的 permission layer 负责 allow / ask / deny 判断。写文件、执行命令和外部工具默认更保守，目的是让用户在关键变更前看到预览和风险。
