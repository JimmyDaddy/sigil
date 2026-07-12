# 常见工作流

[文档首页](README.md) · [English](../en/workflows.md)

这些例子用于日常仓库开发。默认你已经在 TUI 中运行：

```bash
cd /path/to/workspace
sigil
```

## 探索陌生仓库

从只读问题开始：

```text
解释这个仓库的结构。指出主要入口、测试布局、配置文件和用户文档。
```

继续问一个聚焦问题：

```text
解释 Sigil 如何处理这个请求。指出用户可见阶段、可能需要查看的文件，以及它会在哪里显示错误或批准。
```

好的信号：

- Sigil 会说明它读取了哪些具体文件。
- Tool activity 保持只读。
- 你可以要求它继续缩小到某个组件、目录或路径。

## 安全地做小改动

给出目标、范围和验证期望：

```text
更新 docs/zh-CN/quickstart.md，让首次运行路径对新用户更清晰。
这次只改文档。
编辑后检查链接，并运行可用的静态文档检查。
```

出现 approval 时，先检查 diff 再允许。运行结束后：

```bash
git diff
```

如果编辑范围太大，deny 并重新说明范围。

## 计划较大的功能或重构

跨多个文件或需要持久分步骤的任务使用 `/task`：

```text
/task add a troubleshooting section for terminal copy failures and link it from the TUI guide
```

先 review 生成的 task plan，再让 execution 继续。你可以指导下一步：

```text
保持 docs-only，不要编辑 Rust code。同步更新英文和中文文档。
```

如果无需额外指导：

```text
/task continue
```

## 调试失败命令

贴出失败命令和相关输出：

```text
cargo test 失败。断言显示 help text 缺少 Alt-D。
找出 help text 来源，解释可能原因，并在编辑前给出最小修复方案。
```

更安全的做法是先要求 inspect，再决定是否编辑：

```text
先读取失败测试和实现。总结 root cause，等我确认后再改文件。
```

## Review 本地改动

明确要求 review stance：

```text
Review 当前 diff，重点看用户可见回归、过期文档和缺失验证。按严重程度列出发现，并带文件引用。
```

再决定是否修复：

```text
只修复高严重程度的文档问题。不要触碰无关 Rust 改动。
```

## 恢复历史工作

使用：

```text
/resume
```

从列表中选择 session。恢复会重建可见对话和 durable task state。中断工具会显示为 interrupted；Sigil 不会静默重放它们。

如果最新计划任务还没完成：

```text
/task continue
```

或直接在 composer 中输入下一步指导。

## 使用 Code Intelligence

在配置中启用：

```toml
[code_intelligence]
enabled = true
server_startup = "lazy"
```

在 TUI 中使用：

```text
Alt-D
```

对 git changed source files 运行 diagnostics。LSP server 可用时，code intelligence 还可以提供 symbols、definitions、references、code actions 和 rename previews。

如果没有 LSP server，普通 chat 和文件工具仍可使用。见 [Sigil 配置指南](configuration.md) 和 [排障](troubleshooting.md)。

## 用 MCP 连接外部工具

当 Sigil 需要工具化访问外部系统或专门的本地能力时，使用 MCP。

典型流程：

1. 在 `[[mcp_servers]]` 配置 server。
2. 设置保守的 trust policy。
3. 先保持 `approval_default = "ask"`。
4. 用 `/doctor` 检查 command 和 trust 配置。
5. 只有在理解 server 能访问什么之后，再让 Sigil list 和 call MCP tools。

见 [Sigil MCP 接入指南](mcp.md)。

## 更有效的 Prompt 模式

好的 prompt 通常包含：

- 明确目标。
- 相关文件、模块或命令。
- 不要触碰什么。
- 如何验证结果。
- 希望 Sigil 先提出方案，还是直接编辑。

示例：

```text
改进 docs/zh-CN/configuration.md，让新用户更容易理解。
保留 provider-specific advanced fields，但把常见路径放在完整 reference 前面。
必要时同步英文 mirror。
编辑后运行 docs link/path 检查。
```

## 需要你自己 review 的内容

Sigil 可以 inspect、edit 和 run commands，但你仍需要检查：

- `git diff`
- 生成或修改的测试
- 命令输出
- approval diffs
- 可能包含 secret 的配置文件
- 允许 secret egress 或 write action 前的 MCP server
