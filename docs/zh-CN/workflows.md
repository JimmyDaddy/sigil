<!-- public-doc-role: workflows; authority: task-workflow-authority; sections: explore-an-unfamiliar-repository,make-a-small-change-safely,plan-a-larger-feature-or-refactor,debug-a-failing-command,review-local-changes,resume-previous-work,use-code-intelligence,connect-external-tools-with-mcp,what-to-review-yourself; cta: use-cookbook -->

# 常见工作流

[文档首页](README.md) · [任务手册](cookbook.md) · [English](../en/workflows.md)

这些工作流说明完成任务时应该在哪些节点停下来检查、哪些决定需要由你做出。可以直接复制的提示词集中在[任务手册](cookbook.md)。

## 探索陌生仓库

要求 Sigil 保持只读，并指出代码入口、测试、配置与用户文档。可靠的结果应该引用具体文件，同时说明仍不确定的地方。开始修改前，先把下一轮问题缩小到一个目录或一种行为。

## 安全地做小改动

说明目标、允许修改的文件、不能触碰的内容和验证方式。允许编辑前先检查差异预览。完成后运行 `git diff` 和最小相关检查；如果方案超出范围，先拒绝，再重新缩小任务。

## 计划较大的功能或重构

想先得到只读计划时使用 `/plan <prompt>`，只有步骤和边界都合适时才接受“计划就绪”卡片。已经确定要执行多步骤任务时使用 `/task <任务>`；继续最近的任务且没有补充要求时，使用 `/task continue`。

指导语要具体，例如：

```text
只修改文档。英文和中文一起更新。不要修改 Rust 代码。
```

## 调试失败命令

提供命令、相关输出和预期行为。先让 Sigil 阅读失败测试与实现、解释可能原因，并在编辑前等待。原因明确后，再要求最小修复并重跑同一项失败检查。

## 审查本地改动

要求按严重程度列出问题，并附上文件位置。明确指出要处理哪些问题，同时排除工作区里无关的改动。修复后重新检查当前差异，不要只依赖之前的报告。

## 恢复历史工作

打开 `/resume`、选择会话，并先阅读恢复的上下文。中断的工具仍会显示为已中断，不会自动重跑。你可以在输入框中补充新要求，或对未完成任务使用 `/task continue`。

## 使用代码智能

启用后，代码智能可以辅助查找符号、定义与引用，也能提供诊断、代码操作和重命名预览。按 `Alt-D` 检查已修改的源码。如果没有可用的语言服务器，普通对话和文件工具仍可工作；设置见[配置](configuration.md)，问题见[故障排查](troubleshooting.md)。

## 用 MCP 连接外部工具

先配置一个服务端，采用保守的信任设置，再运行 `/doctor`。允许调用或传递凭据前，务必确认这个服务能访问哪些数据。设置与认证方式统一在 [MCP 指南](mcp.md)说明。

## 需要你亲自检查的内容

始终检查最终的文件差异、测试变化、命令输出、可能包含密钥的配置文件，以及任何获准接收数据的外部服务。

<!-- public-doc-cta: use-cookbook -->
下一步：[打开任务手册，获取可复制的提示词](cookbook.md)。
