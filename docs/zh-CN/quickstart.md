# 快速上手

[文档首页](README.md) · [English](../en/quickstart.md)

这份指南使用推荐的 npm alpha 路径，帮助你在真实仓库中完成一次有用的 Sigil TUI 会话。其他安装渠道以及全部更新、卸载说明统一见[安装](installation.md)。

## 开始前

你需要：

- 一个现代终端模拟器。
- 推荐安装路径所需的 Node.js 和 npm。
- 一个模型 provider 凭据。

为了更容易确认效果，第一次建议在一个可以随时查看 `git diff` 的仓库中使用。

## 1. 安装 Sigil

通过 scoped npm package 安装当前 alpha：

```bash
npm install -g @sigil-ai/sigil@alpha
```

Homebrew、Cargo、源码构建、release archive、更新和卸载命令只在[安装](installation.md)中维护，避免多处副本失同步。

确认 binary 可用：

```bash
sigil --version
```

如果 shell 找不到 `sigil`，检查安装器输出，并确认对应 binary 目录在 `PATH` 中。

## 2. 在要编辑的 workspace 中启动

进入你希望 Sigil 操作的项目：

```bash
cd /path/to/workspace
sigil
```

当配置使用常规的 `workspace.root = "."` 时，Sigil 会把这个启动目录视为 active workspace。

## 3. 完成 Quick Setup

如果没有可用配置，Sigil 会打开 Quick Setup。确认：

1. Workspace：希望 Sigil 读取和修改的仓库或目录。
2. Provider/model：Sigil 使用的后端模型。
3. Authentication：API key 或等价凭据。

临时本地使用时，先选择 provider，再按[认证映射](providers.md#认证优先级)设置对应环境变量，然后启动 `sigil`。每个 provider 专页都有准确的 shell 命令；Sigil 不使用一个对所有 provider 通用的 API key 环境变量。

如果通过 Quick Setup 或 `/config` 保存 API key，它会以明文写入本地配置文件。不要提交真实 `sigil.toml`。

## 4. 跑第一轮检查

在 TUI 中运行：

```text
/doctor
```

它会检查 config loading、workspace、sessions、provider/auth、MCP、code intelligence 和 terminal compatibility。

然后先问一个只读仓库问题：

```text
解释这个仓库的结构。指出 main binary、TUI crate、runtime crate、provider crates 和用户文档分别在哪里。
```

只读文件和搜索工具通常不需要审批。这个任务适合第一次试用，因为你可以观察 Sigil 如何读取上下文，而不会立即修改文件。

## 5. 尝试一个小的安全任务

使用窄范围、容易 review 的 prompt：

```text
Review README 和 docs index，找出不清晰的用户文案。先给建议，不要编辑文件。
```

然后再要求一个小编辑：

```text
只应用你刚才提出的 README 文案修改。
```

当 Sigil 请求文件修改工具时，检查：

- 工具摘要。
- 受影响文件。
- diff preview。
- allow/deny action。

审批执行后，用普通 git 工具检查仓库：

```bash
git diff
```

## 6. 大一点的任务使用计划

跨多个文件或需要顺序执行的任务，可以直接用：

```text
/task improve installation docs for macOS, Linux, and Windows users
```

Sigil 会写入 durable task plan。你可以在 composer 里指导或纠正下一步。如果不需要额外说明，使用：

```text
/task continue
```

如果想先做只读规划，再决定是否执行，可以运行 `/plan <prompt>`，并只在需要创建并运行 durable task 时接受 plan-ready 卡片。计划任务状态会写入 append-only control records，重新打开 session 后可以恢复。

## 7. 干净地结束一次 session

提交 Sigil 产出的改动前：

```bash
git diff
sigil doctor
```

按项目需要运行对应测试或 formatter。Sigil 可以在获得允许后运行命令，但你仍应该检查最终 diff 和测试输出。

## 下一步

- 学习日常操作：[Sigil TUI 使用指南](user-guide.md)。
- 选择其他安装渠道或管理已有安装：[安装](installation.md)。
- 参考真实任务模式：[常见工作流](workflows.md)。
- 选择模型后端和认证方式：[Sigil Provider 指南](providers.md)。
- 调整共享的 workspace、权限和工具行为：[Sigil 配置指南](configuration.md)。
- 处理常见问题：[排障](troubleshooting.md)。
