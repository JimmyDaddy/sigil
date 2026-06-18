# Quickstart

[Docs home](README.md) · [简体中文](../zh-CN/quickstart.md)

This guide gets you from a checkout to a useful Sigil session. It is written for users who want to try Sigil in a real repository, not for maintainers changing Sigil itself.

## Before You Begin

You need:

- A modern terminal emulator.
- A Rust toolchain with `cargo`.
- A checkout of this repository.
- A model provider credential.

For the smoothest first run, start in a repository where you can inspect `git diff` before and after the session.

## 1. Install Sigil

Run this from the Sigil repository root:

```bash
cargo install --path crates/sigil --locked
```

Confirm the binary is on `PATH`:

```bash
sigil --version
```

If your shell cannot find `sigil`, make sure Cargo's binary directory is on `PATH`. On macOS and Linux it is usually `~/.cargo/bin`.

## 2. Start In The Workspace You Want To Edit

Open the project you want Sigil to operate on:

```bash
cd /path/to/workspace
sigil
```

Sigil treats this launch directory as the active workspace when the config uses `workspace.root = "."`, which is the normal Quick Setup result.

## 3. Complete Quick Setup

If no usable config exists, Sigil opens Quick Setup. Confirm:

1. Workspace: the repository or directory you want Sigil to inspect and modify.
2. Provider/model: the backend model Sigil should use.
3. Authentication: the API key or equivalent credential.

For temporary local use, you can provide the key before launch:

```bash
export SIGIL_API_KEY="sk-..."
sigil
```

If you save an API key through Quick Setup or `/config`, it is written as plaintext to the local config file. Do not commit a real `sigil.toml`.

## 4. Run The First Checks

Inside the TUI, run:

```text
/doctor
```

This reports config loading, workspace, sessions, provider/auth, MCP, code intelligence, and terminal compatibility.

Then ask a read-only repository question:

```text
Explain this repository structure. Point out the main binary, TUI crate, runtime crate, provider crates, and where user docs live.
```

Read-only file and search tools usually run without approval. This is a good first test because it lets you watch how Sigil reads context before making changes.

## 5. Try A Small Safe Task

Use a narrow, reviewable prompt:

```text
Review the README and docs index for unclear user-facing wording. Suggest improvements first; do not edit files yet.
```

Then ask for a small edit:

```text
Apply only the README wording changes you just proposed.
```

When Sigil requests a file-changing tool, review:

- The tool summary.
- The affected file list.
- The diff preview.
- The allow/deny action.

After approval, inspect your repository normally:

```bash
git diff
```

## 6. Use Planning For Larger Work

For work that needs multiple steps, start with:

```text
/plan improve installation docs for macOS, Linux, and Windows users
```

Sigil writes a durable task plan. You can guide or correct the next step in the composer. To continue without extra guidance:

```text
/plan continue
```

Planned task state is stored in append-only control records and is restored when you reopen the session.

## 7. End A Session Cleanly

Before committing work produced with Sigil:

```bash
git diff
sigil doctor
```

Run project-specific tests or formatters as appropriate. Sigil can run commands when allowed, but you should still review the final diff and test output.

## Next

- Learn daily controls in [user-guide.md](user-guide.md).
- Browse practical task patterns in [workflows.md](workflows.md).
- Tune provider and permission behavior in [configuration.md](configuration.md).
- Fix common setup issues in [troubleshooting.md](troubleshooting.md).
