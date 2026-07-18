<!-- public-doc-role: cookbook; authority: copyable-prompts; sections: explore-a-repository,make-a-small-docs-change,make-a-small-code-change,review-a-diff,fix-a-failing-test,improve-documentation-structure,use-planning,work-with-mcp,investigate-terminal-issues,ask-for-proposal-first,guard-rails-to-add-to-prompts; cta: apply-prompt -->

# 任务手册

[文档首页](README.md) · [常见工作流](workflows.md) · [English](../en/cookbook.md)

下面的提示词可以直接作为起点。使用前，请根据自己的仓库调整文件路径、任务范围和验证命令。

## 探索仓库

```text
像给新贡献者讲解一样解释这个仓库结构。指出重要组件、测试布局、配置文件、用户文档和可能的起点。
```

```text
追踪 <功能或命令> 的执行路径。列出你读取的文件、关键状态变化，以及错误会在哪里展示给用户。
```

## 做一个小文档改动

```text
改进 docs/zh-CN/quickstart.md，让第一次使用路径更清楚。
这次只改文档。
不要修改 Rust 代码。
编辑后检查 Markdown 链接，并运行仓库提供的 Pages 检查。
```

## 做一个小代码改动

```text
在 <具体模块> 中实现 <具体行为>。
编辑前先阅读当前测试，并说明最小改动。
编辑后先运行窄范围相关测试。
```

## 审查文件差异

```text
审查当前文件差异，重点检查缺陷、用户可见回归、过期文档和缺失测试。
按严重程度列出问题，并附上文件位置。
这次审查不要编辑文件。
```

## 修复失败测试

```text
命令 `<command>` 失败，输出如下：

<paste output>

从实现和测试中找出根本原因。先解释原因，再应用最小修复并重新运行失败命令。
```

## 改进文档结构

```text
从新用户视角审查用户文档。
指出阅读路径哪里不清晰、哪里重复、哪里需要例子降低理解成本。
然后一致更新英文和中文文档。
```

## 使用计划模式

```text
/plan 在不改变产品行为的前提下，把配置指南拆成首次设置、常见任务和完整参考三部分
```

继续指导：

```text
只修改文档。英文和中文必须同步更新。每次调整结构后都运行文档检查。
```

## 处理 MCP

```text
检查已经配置的 MCP 服务，说明有哪些工具可用、采用什么信任级别，以及哪些操作需要审批。
先不要调用任何外部工具。
```

## 排查终端问题

```text
根据 Doctor 输出解释为什么 OSC52 复制或鼠标捕获在这个终端里不工作。推荐最小的配置修改，并链接到终端兼容性检查清单。
```

## 要求先出方案

```text
编辑前先提出一个小实施计划。包含文件、预计测试和风险。等我确认后再改。
```

## 可加到提示词里的边界

需要时加上这些行：

```text
只处理这些路径：<路径>。
不要修改无关文件。
不要提交。
优先沿用仓库中的现有模式。
这是纯文档改动，只运行文档相关检查。
如果工具需要写入权限，应用改动前先展示文件差异。
```

<!-- public-doc-cta: apply-prompt -->
下一步：[选择对应的工作流](workflows.md)。
