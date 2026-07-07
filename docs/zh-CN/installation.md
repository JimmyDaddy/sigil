# 安装

[文档首页](README.md) · [快速上手](quickstart.md) · [English](../en/installation.md)

本文说明首个 release 的安装路径。如果你想按首次使用流程走一遍，先看 [quickstart.md](quickstart.md)。`v0.0.1-alpha` 是 early preview，不承诺配置、插件、高级 sandbox 行为或自动化入口的稳定兼容。

## 前置条件

- 一个现代终端模拟器。
- 一种安装器：npm、Homebrew，或通过 `rustup` / 系统包安装的 Rust toolchain。
- 一个模型 provider 凭据。首次启动时可以通过 Quick Setup 填写。

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
cargo install --git https://github.com/JimmyDaddy/sigil --tag v0.0.1-alpha --locked sigil
```

这会把 `sigil` binary 安装到 Cargo 的 binary 目录。macOS 和 Linux 默认是 `~/.cargo/bin`，Windows 默认是 `%USERPROFILE%\.cargo\bin`。

crates.io 上 `sigil` 包名已被其他 crate 使用，因此 crates.io 分发需要后续再决定 package name；binary 仍然可以保持 `sigil`。

## 从 checkout 安装

本地开发时，在仓库根目录运行：

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

## 构建 Release Archive

维护者可以从 checkout 构建本地 release archive：

```bash
scripts/build-release-archive.sh
```

脚本会用 release mode 构建 `sigil`、注入构建元数据、对构建出的 binary 运行 `sigil --version` 和 `sigil doctor`，然后写出：

```text
dist/sigil-<version>-<target>.tar.gz
dist/sigil-<version>-<target>.tar.gz.sha256
```

需要指定 Rust target triple 时：

```bash
scripts/build-release-archive.sh --target aarch64-apple-darwin
```

archive 内包含 `sigil` binary、README、logo assets 和安装文档。tagged release 会由 release workflow 构建，并附带 checksum、GitHub artifact provenance attestation、生成的 release notes、供 tap 维护者使用的 `sigil-ai.rb` Homebrew formula asset，以及从 release archives 生成的 npm package tarballs。自更新仍是后续工作。

## 更新

使用原来的安装器更新：

```bash
npm install -g @sigil-ai/sigil@alpha
brew upgrade sigil-ai
cargo install --git https://github.com/JimmyDaddy/sigil --tag v0.0.1-alpha --locked sigil --force
cargo install --path crates/sigil --locked --force
```

## 卸载

使用对应卸载命令：

```bash
npm uninstall -g @sigil-ai/sigil
brew uninstall sigil-ai
cargo uninstall sigil
```

## 开发运行

修改仓库代码时，如果不想重新安装，可以在 checkout 内直接运行：

```bash
cargo run -p sigil
cargo run -p sigil -- doctor
```

这些命令是开发快捷路径。面向用户的文档应优先使用安装后的 `sigil` 命令。
