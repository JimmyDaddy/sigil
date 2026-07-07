# Installation

[Docs home](README.md) · [Quickstart](quickstart.md) · [简体中文](../zh-CN/installation.md)

This guide covers the first-release install paths. If you want a first-run walkthrough, start with [quickstart.md](quickstart.md). `v0.0.1-alpha` is an early preview, not a stable compatibility promise for config, plugins, advanced sandbox behavior, or automation surfaces.

## Requirements

- A modern terminal emulator.
- One installer: npm, Homebrew, or a Rust toolchain installed through `rustup` or an equivalent system package.
- A model provider credential. Quick Setup can collect it on first launch.

## Install With npm

The npm package is scoped as `@sigil-ai/sigil`. It installs a small Node.js launcher plus a platform-specific optional binary package. The installed command is still `sigil`.

```bash
npm install -g @sigil-ai/sigil@alpha
```

Confirm the install:

```bash
sigil --version
sigil doctor
```

The unscoped npm package name `sigil` is not the first-release package name.

## Install With Homebrew

The Homebrew path uses a dedicated tap formula named `sigil-ai` to avoid confusing this project with other Homebrew software named Sigil. The formula installs the `sigil` binary.

```bash
brew install JimmyDaddy/sigil/sigil-ai
```

Confirm the install:

```bash
sigil --version
sigil doctor
```

The release workflow generates `sigil-ai.rb` from the macOS release archives. The formula is published in the `JimmyDaddy/homebrew-sigil` tap.

## Install With Cargo

For the first release, Cargo installs from the Git tag rather than crates.io:

```bash
cargo install --git https://github.com/JimmyDaddy/sigil --tag v0.0.1-alpha --locked sigil
```

This installs the `sigil` binary into Cargo's binary directory. The default is `~/.cargo/bin` on macOS and Linux, and `%USERPROFILE%\.cargo\bin` on Windows.

The crates.io package name `sigil` is already used by another crate, so crates.io distribution needs a later package-name decision. The binary can still remain `sigil`.

## Install From A Checkout

For local development, run these commands from a repository checkout:

```bash
cargo install --path crates/sigil --locked
```

Confirm the install:

```bash
sigil --version
sigil doctor
```

## Start

For normal use, open the repository or workspace you want Sigil to operate on and start the TUI there:

```bash
cd /path/to/workspace
sigil
```

If no usable config exists, Sigil opens Quick Setup. After setup, `workspace.root = "."` means the directory where you launched `sigil` is the active workspace.

Use explicit subcommands only for automation, diagnostics, or scripts:

```bash
sigil doctor
sigil run "summarize this repository"
```

Inside the TUI, `/doctor` renders the same diagnostics report in the transcript.

## Build A Release Archive

Maintainers can build a local release archive from the checkout:

```bash
scripts/build-release-archive.sh
```

The script builds `sigil` in release mode, injects build metadata, runs `sigil --version` and `sigil doctor` against the built binary, then writes:

```text
dist/sigil-<version>-<target>.tar.gz
dist/sigil-<version>-<target>.tar.gz.sha256
```

Build for an explicit Rust target triple when needed:

```bash
scripts/build-release-archive.sh --target aarch64-apple-darwin
```

The archive contains the `sigil` binary plus the README files, logo assets, and installation docs.
Tagged releases are built by the release workflow and include checksums, GitHub artifact provenance attestations, generated release notes, a `sigil-ai.rb` Homebrew formula asset for tap maintainers, and npm package tarballs generated from the release archives. Self-update is still future work.

## Update

Use the installer you used originally:

```bash
npm install -g @sigil-ai/sigil@alpha
brew upgrade sigil-ai
cargo install --git https://github.com/JimmyDaddy/sigil --tag v0.0.1-alpha --locked sigil --force
cargo install --path crates/sigil --locked --force
```

## Uninstall

Use the matching uninstall command:

```bash
npm uninstall -g @sigil-ai/sigil
brew uninstall sigil-ai
cargo uninstall sigil
```

## Development Runs

When you are changing the repository and do not want to reinstall, run directly from the checkout:

```bash
cargo run -p sigil
cargo run -p sigil -- doctor
```

These commands are development shortcuts. User-facing docs should prefer the installed `sigil` command.
