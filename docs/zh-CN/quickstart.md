# 快速上手

[文档首页](README.md) · [English](../en/quickstart.md)

这份指南帮助你从 checkout 安装 Sigil，并完成一次真实有用的 TUI session。它面向想试用 Sigil 的用户，而不是修改 Sigil 本身的维护者。

## 开始前

你需要：

- 一个现代终端模拟器。
- 带 `cargo` 的 Rust toolchain。
- 本仓库的 checkout。
- 一个模型 provider 凭据。

为了更容易确认效果，第一次建议在一个可以随时查看 `git diff` 的仓库中使用。

## 1. 安装 Sigil

在 Sigil 仓库根目录运行：

```bash
cargo install --path crates/sigil --locked
```

确认 binary 可用：

```bash
sigil --version
```

如果 shell 找不到 `sigil`，确认 Cargo binary 目录在 `PATH` 中。macOS 和 Linux 通常是 `~/.cargo/bin`。

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

临时本地使用可以在启动前提供 key：

```bash
export SIGIL_API_KEY="sk-..."
sigil
```

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

跨多个文件或需要顺序执行的任务，用：

```text
/task improve installation docs for macOS, Linux, and Windows users
```

Sigil 会写入 durable task plan。你可以在 composer 里指导或纠正下一步。如果不需要额外说明，使用：

```text
/task continue
```

计划任务状态会写入 append-only control records，重新打开 session 后可以恢复。

## 7. 干净地结束一次 session

提交 Sigil 产出的改动前：

```bash
git diff
sigil doctor
```

按项目需要运行对应测试或 formatter。Sigil 可以在获得允许后运行命令，但你仍应该检查最终 diff 和测试输出。

## 下一步

- 学习日常操作：[user-guide.md](user-guide.md)。
- 参考真实任务模式：[workflows.md](workflows.md)。
- 调整 provider 和权限：[configuration.md](configuration.md)。
- 处理常见问题：[troubleshooting.md](troubleshooting.md)。
