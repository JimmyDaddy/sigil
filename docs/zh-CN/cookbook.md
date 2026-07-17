<!-- public-doc-role: cookbook; authority: copyable-prompts; sections: explore-a-repository,make-a-small-docs-change,make-a-small-code-change,review-a-diff,fix-a-failing-test,improve-documentation-structure,use-planning,work-with-mcp,investigate-terminal-issues,ask-for-proposal-first,guard-rails-to-add-to-prompts; cta: apply-prompt -->

# Cookbook

[文档首页](README.md) · [常见工作流](workflows.md) · [English](../en/cookbook.md)

这些 prompt 可以作为起点。请按你的仓库调整文件路径、范围和验证命令。

## 探索仓库

```text
像给新贡献者讲解一样解释这个仓库结构。指出重要组件、测试布局、配置文件、用户文档和可能的起点。
```

```text
追踪 <feature or command> 的执行路径。列出你读取的文件、状态转换，以及错误在哪里展示给用户。
```

## 做一个小文档改动

```text
改进 docs/zh-CN/quickstart.md，让第一次使用路径更清楚。
这次只改文档。
不要修改 Rust code。
编辑后检查 Markdown links，并运行可用的 Pages check。
```

## 做一个小代码改动

```text
在 <specific module> 实现 <specific behavior>。
编辑前先阅读当前测试，并说明最小改动。
编辑后先运行窄范围相关测试。
```

## Review Diff

```text
Review 当前 diff，重点看 bugs、用户可见回归、过期文档和缺失测试。
按严重程度列 findings，并带文件引用。
这次 review 不要编辑文件。
```

## 修复失败测试

```text
命令 `<command>` 失败，输出如下：

<paste output>

从实现和测试里找 root cause。先解释原因，再应用最小修复并重跑失败命令。
```

## 改进文档结构

```text
以新用户视角 review 用户文档。
指出阅读路径哪里不清晰、哪里重复、哪里需要例子降低理解成本。
然后一致更新英文和中文文档。
```

## 使用 Planning

```text
/plan split the configuration guide into first-run setup, common tasks, and full reference without changing product behavior
```

继续指导：

```text
保持 docs-only。更新英文和中文 mirror。每次结构改动后运行 docs checks。
```

## 处理 MCP

```text
检查已配置的 MCP servers，说明哪些 tools 可用、使用什么 trust class、哪些动作需要 approval。
先不要调用外部 tools。
```

## 排查终端问题

```text
根据 doctor output 解释为什么 OSC52 copy 或 mouse capture 在这个终端里不工作。推荐最小配置修改，并指向 terminal compatibility checklist。
```

## 要求先出方案

```text
编辑前先提出一个小实施计划。包含文件、预计测试和风险。等我确认后再改。
```

## 可加到 Prompt 里的护栏

需要时加上这些行：

```text
Scope only: <paths>.
Do not touch unrelated files.
Do not commit.
Prefer existing patterns in this repository.
Run only docs checks; this is a docs-only change.
If a tool needs write access, show the diff before applying it.
```

<!-- public-doc-cta: apply-prompt -->
下一步：[选择对应的工作流](workflows.md)。
