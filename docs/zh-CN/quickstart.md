<!-- public-doc-role: quickstart; authority: first-success; sections: before-you-begin,1-install-sigil,2-start-in-the-workspace-you-want-to-edit,3-complete-quick-setup,4-run-the-first-checks,5-try-a-small-safe-task; cta: continue-by-task -->

# 快速开始

[文档首页](README.md) · [安装](installation.md) · [English](../en/quickstart.md)

按照本页操作，你会安装 Sigil、打开一个真实仓库，并在检查完第一个小改动后结束。其他安装方式以及更新、卸载命令统一放在[安装指南](installation.md)。

## 开始前

你需要一个现代终端、Node.js 与 npm、一份模型服务凭据，以及一个可以查看 `git diff` 的仓库。

## 1. 安装 Sigil

```bash
npm install -g @sigil-ai/sigil@alpha
sigil --version
```

如果找不到命令，请检查安装输出，并确认 npm 的可执行文件目录已经加入 `PATH`。

## 2. 在要编辑的工作区中启动

```bash
cd /path/to/workspace
sigil
```

快速设置保存 `workspace.root = "."` 后，启动 Sigil 时所在的目录就会成为当前工作区。

## 3. 完成快速设置

缺少配置时，先确认工作区，再选择模型服务和具体模型，最后填写认证信息。临时使用时，优先采用[模型服务指南](providers.md#认证优先级)列出的环境变量。通过快速设置或 `/config` 保存的密钥会以明文写入本机配置文件；不要把包含真实凭据的 `sigil.toml` 提交到仓库。

## 4. 跑第一轮检查

运行：

```text
/doctor
```

然后提出只读问题：

```text
解释这个仓库的结构，指出主要目录、测试、配置文件和用户文档。不要修改文件。
```

结果应引用具体文件，只出现只读活动，不应要求变更审批。

## 5. 尝试一个小的安全任务

先要求给出方案：

```text
检查 README 中不清楚的用户文案。先提出改进建议，不要修改文件。
```

再要求一次小改动：

```text
只应用刚才提出的 README 文案修改。
```

允许变更前，检查摘要、受影响文件和差异。完成后再亲自检查仓库：

```bash
git diff
```

按项目需要运行格式化工具或测试。多步骤工作继续阅读[常见工作流](workflows.md)；日常操作见 [TUI 用户指南](user-guide.md)。

<!-- public-doc-cta: continue-by-task -->
下一步：[继续阅读用户指南](user-guide.md)。
