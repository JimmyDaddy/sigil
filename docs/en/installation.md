# Installing From Source

[简体中文](../zh-CN/installation.md)

This guide covers the current supported install path from a local repository checkout. Release archives, package managers, and self-update are intentionally not part of this path yet.

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
