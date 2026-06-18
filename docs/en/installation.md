# Installing From Source

[Docs home](README.md) · [Quickstart](quickstart.md) · [简体中文](../zh-CN/installation.md)

This guide covers the current supported install path from a local repository checkout. If you want a first-run walkthrough, start with [quickstart.md](quickstart.md). Release archives can be built locally for validation, but package managers and self-update are not part of this path yet.

## Requirements

- Rust toolchain installed through `rustup` or an equivalent system package.
- A Sigil repository checkout.
- Cargo's binary directory on `PATH`. The default is `~/.cargo/bin` on macOS and Linux, and `%USERPROFILE%\.cargo\bin` on Windows.

## Install

Run these commands from the repository root:

```bash
cargo install --path crates/sigil --locked
```

This installs the `sigil` binary. Running `sigil` without a subcommand opens the TUI. Automation and diagnostics live behind explicit subcommands.

Confirm the installed binary:

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
Tagged releases are built by the release workflow and include checksums,
GitHub artifact provenance attestations, generated release notes, and a
`sigil.rb` Homebrew formula asset for tap maintainers. Self-update is still
future work.

## Update

From an updated checkout, reinstall with `--force`:

```bash
cargo install --path crates/sigil --locked --force
```

## Uninstall

Uninstall by package name:

```bash
cargo uninstall sigil
```

`cargo uninstall sigil` removes the `sigil` binary.

## Development Runs

When you are changing the repository and do not want to reinstall, run directly from the checkout:

```bash
cargo run -p sigil
cargo run -p sigil -- doctor
```

These commands are development shortcuts. User-facing docs should prefer the installed `sigil` command.
