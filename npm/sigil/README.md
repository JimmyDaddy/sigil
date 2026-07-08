# @sigil-ai/sigil

This package installs the `sigil` command for the current platform.

The package is a small Node.js launcher. The actual Rust binary is published in
platform-specific optional packages generated from the GitHub release archives.

## Install

```bash
npm install -g @sigil-ai/sigil@alpha
sigil --version
sigil doctor
```

## Maintainers

Do not publish this directory directly. Generate publishable package directories
from release archives:

```bash
scripts/prepare-npm-packages.sh --version 0.0.1-alpha.1 --dist-dir dist --out-dir dist/npm-packages
```
