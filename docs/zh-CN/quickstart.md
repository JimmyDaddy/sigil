<!-- public-doc-role: quickstart; authority: first-success; sections: before-you-begin,1-install-sigil,2-start-in-the-workspace-you-want-to-edit,3-complete-quick-setup,4-run-the-first-checks,5-try-a-small-safe-task; cta: continue-by-task -->

# 快速开始

[文档首页](README.md) · [安装](installation.md) · [English](../en/quickstart.md)

这条路径会安装 Sigil、打开真实仓库，并在检查完一个小改动后结束。其他软件包、更新和卸载方式只在[安装指南](installation.md)维护。

## 开始前

你需要现代终端、Node.js 与 npm、一个模型 provider 凭据，以及一个可以检查 `git diff` 的仓库。

## 1. 安装 Sigil

```bash
npm install -g @sigil-ai/sigil@alpha
sigil --version
```

如果找不到命令，请检查安装器输出，并确认 npm 的 binary 目录在 `PATH` 中。

## 2. 在要编辑的 Workspace 中启动

```bash
cd /path/to/workspace
sigil
```

Quick Setup 保存 `workspace.root = "."` 后，启动目录会成为常用的活动 workspace。

## 3. 完成 Quick Setup

缺少配置时，确认 workspace、选择 provider 和 model，再填写认证信息。临时使用优先选择 [Provider 指南](providers.md#认证优先级)列出的环境变量。通过 Quick Setup 或 `/config` 保存的 key 会以明文写入本机配置文件；不要提交真实的 `sigil.toml`。

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

允许变更前，检查摘要、受影响文件和 diff。最后自己检查仓库：

```bash
git diff
```

按项目需要运行 formatter 或测试。多步骤工作继续阅读[常见工作流](workflows.md)；日常操作见 [TUI 用户指南](user-guide.md)。

<!-- public-doc-cta: continue-by-task -->
下一步：[继续阅读用户指南](user-guide.md)。
