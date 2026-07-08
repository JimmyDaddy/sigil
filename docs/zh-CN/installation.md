# 安装

[文档首页](README.md) · [快速上手](quickstart.md) · [English](../en/installation.md)

本文说明首个 release 的安装路径。如果你想按首次使用流程走一遍，先看 [quickstart.md](quickstart.md)。`v0.0.1-alpha.1` 是 early preview，不承诺配置、插件、高级 sandbox 行为或自动化入口的稳定兼容。

## 前置条件

- 一个现代终端模拟器。
- 一种安装器：npm、Homebrew，或通过 `rustup` / 系统包安装的 Rust toolchain。
- 一个模型 provider 凭据。首次启动时可以通过 Quick Setup 填写。

## 当前安装渠道

| 渠道 | 当前覆盖 | 适合场景 |
| --- | --- | --- |
| npm alpha | `@sigil-ai/sigil@alpha` 背后使用 platform-specific optional binary package。 | 想要最短的跨平台安装路径。 |
| Homebrew tap | macOS formula 位于 `JimmyDaddy/homebrew-sigil`，安装名是 `sigil-ai`，命令仍是 `sigil`。 | 你习惯用 Homebrew 管理终端工具。 |
| Cargo git tag | 使用本地 Rust toolchain 从 tagged Git release 构建。 | 你已有 Rust 工具链，或需要源码构建路径。 |
| GitHub release archive | 提供可下载 release archive 和 checksum 文件。 | 需要手动或离线安装。 |

## 通过 npm 安装

npm 包名使用 scoped package：`@sigil-ai/sigil`。它安装一个很小的 Node.js launcher，并通过 platform-specific optional package 携带实际 binary。最终命令仍然是 `sigil`。

```bash
npm install -g @sigil-ai/sigil@alpha
```

确认安装：

```bash
sigil --version
sigil doctor
```

首个 release 不使用 unscoped npm 包名 `sigil`。

## 通过 Homebrew 安装

Homebrew 使用专用 tap formula `sigil-ai`，避免和其他名为 Sigil 的 Homebrew 软件混淆。formula 安装后的 binary 仍然叫 `sigil`。

```bash
brew install JimmyDaddy/sigil/sigil-ai
```

确认安装：

```bash
sigil --version
sigil doctor
```

release workflow 会从 macOS release archives 生成 `sigil-ai.rb`。该 formula 已发布到 `JimmyDaddy/homebrew-sigil` tap。

## 通过 Cargo 安装

首个 release 通过 Git tag 安装，而不是 crates.io：

```bash
cargo install --git https://github.com/JimmyDaddy/sigil --tag v0.0.1-alpha.1 --locked sigil
```

这会把 `sigil` binary 安装到 Cargo 的 binary 目录。macOS 和 Linux 默认是 `~/.cargo/bin`，Windows 默认是 `%USERPROFILE%\.cargo\bin`。

crates.io 上 `sigil` 包名已被其他 crate 使用，因此 crates.io 分发需要后续再决定 package name；binary 仍然可以保持 `sigil`。

## 从源码安装

如果你希望从本地 checkout 构建，在仓库根目录运行：

```bash
cargo install --path crates/sigil --locked
```

确认安装：

```bash
sigil --version
sigil doctor
```

## 启动

日常使用时，先进入你希望 Sigil 操作的仓库或工作目录，再启动 TUI：

```bash
cd /path/to/workspace
sigil
```

如果没有可用配置，Sigil 会进入 Quick Setup。完成后，`workspace.root = "."` 表示启动 `sigil` 时所在目录就是当前工作区。

显式子命令只用于自动化、诊断或脚本：

```bash
sigil doctor
sigil run "总结一下当前仓库"
```

在 TUI 内也可以用 `/doctor`，同一份诊断报告会渲染到 transcript。

## 从 Release Archive 安装

能使用包管理器时优先使用上面的安装路径。手动安装时，从 [GitHub releases 页面](https://github.com/JimmyDaddy/sigil/releases)下载匹配平台的 archive 和 checksum，校验 checksum，解压 archive，并把 `sigil` binary 放到 `PATH` 中。

archive 内包含 `sigil` binary、用户 README、logo assets 和安装文档。自更新仍属于后续 packaging 工作。

## 更新

使用原来的安装器更新：

```bash
npm install -g @sigil-ai/sigil@alpha
brew upgrade sigil-ai
cargo install --git https://github.com/JimmyDaddy/sigil --tag v0.0.1-alpha.1 --locked sigil --force
cargo install --path crates/sigil --locked --force
```

## 卸载

使用对应卸载命令：

```bash
npm uninstall -g @sigil-ai/sigil
brew uninstall sigil-ai
cargo uninstall sigil
```
