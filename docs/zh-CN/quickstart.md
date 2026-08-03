<!-- public-doc-role: quickstart; authority: first-success; sections: before-you-begin,1-install-sigil,2-start-in-the-workspace-you-want-to-edit,3-complete-quick-setup,4-run-the-first-checks,5-try-a-small-safe-task; cta: continue-by-task -->

# 快速开始

[文档首页](README.md) · [安装](installation.md) · [English](../en/quickstart.md)

按照本页操作，你会安装 Sigil、打开一个真实仓库，并在检查完第一个小改动后结束。其他安装方式以及更新、卸载命令统一放在[安装指南](installation.md)。

## 开始前

你需要一个现代终端、Node.js 与 npm、一份模型服务凭据，以及一个可以查看 `git diff` 的仓库。

## 1. 安装 Sigil

```bash
npm install -g @sigil-ai/sigil@beta
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

缺少配置时，最多做三个主要决定：provider、凭据来源和模型。`Review, trust folder, save and
start` 会确认 route，并允许把启动目录作为工作区。快速设置会写入一个命名的 V2 connection 和
复合保存默认值。
远端模型列表是可选增强；站点不提供列表或刷新失败时，直接输入精确模型 ID 即可继续保存。

在 Sigil Desktop 中先打开项目。新电脑或缺少配置时，会在开放**新建会话**前进入同样的三步
Provider 向导。之后可直接在设置页查看全部已保存连接并添加新连接，不需要先打开一次会话。

普通本机使用请选择受保护凭据存储；粘贴的 key 只在创建凭据记录期间保留，`sigil.toml` 仅保存
随机引用。默认 `file` 会写入 owner-only 的 `~/.sigil/credentials.json`，不会触发系统认证框。
CI 或已由 Shell 管理 secret 时选择允许列表中的环境变量。详见
[模型服务指南](providers.md#认证优先级)。

之后可用 `/model connection-id/model-id` 把空闲的当前会话切换到精确 ready route，并保留对话
历史。在选择器中，Enter 切换当前会话，`D` 只修改保存默认值；在 `/config` 中选择 connection
或模型并保存，则会同时应用到当前对话和未来会话。

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
