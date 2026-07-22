<!-- public-doc-role: quickstart; authority: first-success; sections: before-you-begin,1-install-sigil,2-start-in-the-workspace-you-want-to-edit,3-complete-quick-setup,4-run-the-first-checks,5-try-a-small-safe-task; cta: continue-by-task -->

# Quickstart

[Docs home](README.md) · [Installation](installation.md) · [简体中文](../zh-CN/quickstart.md)

This path installs Sigil, opens a real repository, and ends after one reviewed change. Other package, update, and uninstall paths belong in [Installation](installation.md).

## Before You Begin

You need a modern terminal, Node.js with npm, a model-provider credential, and a repository where you can inspect `git diff`.

## 1. Install Sigil

```bash
npm install -g @sigil-ai/sigil@alpha
sigil --version
```

If the command is missing, check the installer output and make sure npm's binary directory is on `PATH`.

## 2. Start In The Workspace You Want To Edit

```bash
cd /path/to/workspace
sigil
```

The launch directory becomes the normal active workspace when Quick Setup saves `workspace.root = "."`.

## 3. Complete Quick Setup

When configuration is missing, choose the provider first, then choose its model and add authentication. `Trust folder, save and start` is the explicit confirmation that the launch directory may be used as the workspace; there is no separate trust toggle. Press Enter on Provider to switch services in Quick Setup or later in `/config`. For temporary use, prefer the environment variable listed in the [provider guide](providers.md#authentication-priority). A key saved through Quick Setup or `/config` is plaintext in the local config file; never commit a real `sigil.toml`.

## 4. Run The First Checks

Run:

```text
/doctor
```

Then ask a read-only question:

```text
Explain this repository structure. Identify the main directories, tests, configuration files, and user documentation. Do not edit files.
```

You should see concrete files and read-only activity without a change approval.

## 5. Try A Small Safe Task

Start with a proposal:

```text
Review the README for unclear user-facing wording. Suggest improvements first; do not edit files yet.
```

Then request one narrow edit:

```text
Apply only the README wording changes you proposed.
```

Before allowing the change, check the summary, affected files, and diff. Finish by reviewing the repository yourself:

```bash
git diff
```

Run any project-specific formatter or test that the change needs. For multi-step work, continue with [Common workflows](workflows.md); for daily controls, open the [TUI user guide](user-guide.md).

<!-- public-doc-cta: continue-by-task -->
Next: [Continue with the User Guide](user-guide.md).
