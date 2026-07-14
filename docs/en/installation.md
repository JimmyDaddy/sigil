# Installation

[Docs home](README.md) · [Quickstart](quickstart.md) · [简体中文](../zh-CN/installation.md)

This page is the authoritative source for Sigil install channels, update and uninstall commands, and release-archive handling. Other user guides link here instead of copying those details. If you want a first-run walkthrough, start with [Quickstart](quickstart.md). `v0.0.1-alpha.1` is an early preview, not a stable compatibility promise for config, plugins, advanced sandbox behavior, or automation surfaces.

The package-manager and Cargo tag commands below install the published `v0.0.1-alpha.1` release. The GitHub Pages documentation tracks `main`, so features listed under [Unreleased](changelog.md#unreleased-main) may require a source install until the next alpha is tagged.

## Requirements

- A modern terminal emulator.
- One installer: npm, Homebrew, or a Rust toolchain installed through `rustup` or an equivalent system package.
- A model provider credential. Quick Setup can collect it on first launch.

## Supported Install Channels

| Channel | Current coverage | Use when |
| --- | --- | --- |
| npm alpha | Platform-specific optional binary packages behind `@sigil-ai/sigil@alpha`. | You want the shortest cross-platform install path. |
| Homebrew tap | macOS formula in `JimmyDaddy/homebrew-sigil`, installed as `sigil-ai` while exposing the `sigil` command. | You manage terminal tools with Homebrew. |
| Cargo git tag | Builds from the tagged Git release with your local Rust toolchain. | You already use Rust tooling or need a source-based install. |
| GitHub release archive | Downloadable release archives with checksum files. | You need a manual or offline install. |

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
cargo install --git https://github.com/JimmyDaddy/sigil --tag v0.0.1-alpha.1 --locked sigil
```

This installs the `sigil` binary into Cargo's binary directory. The default is `~/.cargo/bin` on macOS and Linux, and `%USERPROFILE%\.cargo\bin` on Windows.

The crates.io package name `sigil` is already used by another crate, so crates.io distribution needs a later package-name decision. The binary can still remain `sigil`.

## Install From Source

If you prefer to build from a local checkout, run this from the repository root:

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

## Install From A Release Archive

Use the package-manager paths above when possible. For a manual install, download the matching archive and checksum from the [GitHub releases page](https://github.com/JimmyDaddy/sigil/releases), verify the checksum, unpack the archive, and place the `sigil` binary on your `PATH`.

The archive contains the `sigil` binary plus the user-facing README, logo assets, and installation docs. Self-update is still future packaging work.

## Update

Use the installer you used originally:

```bash
npm install -g @sigil-ai/sigil@alpha
brew upgrade sigil-ai
cargo install --git https://github.com/JimmyDaddy/sigil --tag v0.0.1-alpha.1 --locked sigil --force
cargo install --path crates/sigil --locked --force
```

## Uninstall

Use the matching uninstall command:

```bash
npm uninstall -g @sigil-ai/sigil
brew uninstall sigil-ai
cargo uninstall sigil
```
