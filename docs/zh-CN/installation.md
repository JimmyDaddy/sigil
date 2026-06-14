# 源码安装

[English](../en/installation.md)

本文说明当前支持的源码 checkout 安装路径。Release archive 可以在本地构建用于验证；包管理器和自更新还不属于这条路径。

## 前置条件

- 已通过 `rustup` 或系统包安装 Rust toolchain。
- 已有 Sigil 仓库 checkout。
- Cargo 的 binary 目录在 `PATH` 里。macOS 和 Linux 默认是 `~/.cargo/bin`，Windows 默认是 `%USERPROFILE%\.cargo\bin`。

## 安装

在仓库根目录运行：

```bash
cargo install --path crates/sigil --locked
```

这会安装 `sigil` binary。直接运行 `sigil` 会打开 TUI；自动化和诊断能力放在显式子命令后面。

确认已安装 binary：

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

archive 内包含 `sigil` binary、README 和安装文档。tagged release 会由 release workflow 构建，并附带 checksum、GitHub artifact provenance attestation、生成的 release notes，以及供 tap 维护者使用的 `sigil.rb` Homebrew formula asset。自更新仍是后续工作。

## 更新

从已更新的 checkout 重新安装时，加 `--force`：

```bash
cargo install --path crates/sigil --locked --force
```

## 卸载

按 package 名卸载：

```bash
cargo uninstall sigil
```

`cargo uninstall sigil` 会移除 `sigil` binary。

## 开发运行

修改仓库代码时，如果不想重新安装，可以在 checkout 内直接运行：

```bash
cargo run -p sigil
cargo run -p sigil -- doctor
```

这些命令是开发快捷路径。面向用户的文档应优先使用安装后的 `sigil` 命令。
