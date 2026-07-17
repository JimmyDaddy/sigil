<!-- public-doc-role: workflows; authority: task-workflow-authority; sections: explore-an-unfamiliar-repository,make-a-small-change-safely,plan-a-larger-feature-or-refactor,debug-a-failing-command,review-local-changes,resume-previous-work,use-code-intelligence,connect-external-tools-with-mcp,what-to-review-yourself; cta: use-cookbook -->

# 常见工作流

[文档首页](README.md) · [Cookbook](cookbook.md) · [English](../en/workflows.md)

这些工作流说明任务中的检查点和用户决策。只包含可复制 prompt 的版本在 [Cookbook](cookbook.md)。

## 探索陌生仓库

要求 Sigil 保持只读，并指出入口、测试、配置与用户文档。有效结果会引用具体文件，并明确不确定之处。要求修改前，先把下一轮问题缩小到一个目录或行为。

## 安全地做小改动

说明目标、允许修改的文件、不能触碰的内容和验证方式。允许编辑前检查审批 diff。最后运行 `git diff` 和最小相关检查；如果方案超出范围，拒绝并重新缩小任务。

## 计划较大的功能或重构

想先得到只读计划时使用 `/plan <prompt>`，只在步骤和边界都合适时接受 Plan ready card。已经确定要执行多步骤任务时使用 `/task <任务>`；最新任务无需新指导时使用 `/task continue`。

指导语要具体，例如：

```text
只修改文档。英文和中文一起更新。不要修改 Rust 代码。
```

## 调试失败命令

提供命令、相关输出和预期行为。先让 Sigil 阅读失败测试与实现、解释可能原因，并在编辑前等待。原因明确后，再要求最小修复并重跑同一项失败检查。

## Review 本地改动

要求按严重程度列出 finding，并附文件位置。明确要处理哪些 finding，同时把无关工作区改动排除在外。修复后重新检查 live diff，不要只依赖早先报告。

## 恢复历史工作

打开 `/resume`、选择 session，并先阅读恢复的上下文。中断工具仍显示为已中断，不会自动重跑。可以在输入框提供新指导，或对未完成任务使用 `/task continue`。

## 使用 Code Intelligence

启用后，code intelligence 可以辅助符号、定义、引用、诊断、code action 和 rename preview。按 `Alt-D` 检查已修改源码。如果没有可用 language server，普通 chat 和文件工具仍可工作；设置见[配置](configuration.md)，问题见[故障排查](troubleshooting.md)。

## 用 MCP 连接外部工具

先配置一个 server，使用保守 trust，运行 `/doctor`，并在允许调用或凭据前确认 server 能访问什么。设置与认证只在 [MCP 指南](mcp.md)说明。

## 需要你自己 Review 的内容

始终检查最终 diff、变化的测试、命令输出、可能包含 secret 的配置文件，以及任何获准接收数据的外部服务。

<!-- public-doc-cta: use-cookbook -->
下一步：[打开 Cookbook 获取可复制提示词](cookbook.md)。
