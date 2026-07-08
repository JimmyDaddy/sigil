# Sigil Release Process

This document describes the maintainer release path. The user-facing install
entrypoint remains `sigil`; package manager wrappers must not introduce a new
product surface.

## Release Trigger

Create and push a version tag that matches the Cargo package version:

```bash
git tag -a v0.0.1-alpha.1 -m "Sigil 0.0.1-alpha.1"
git push origin v0.0.1-alpha.1
```

The `Release` workflow also supports manual dispatch with an existing tag.

## Workflow

The release workflow is `.github/workflows/release.yml`.

1. Build native release archives on Ubuntu, macOS arm64, macOS Intel, and Windows runners.
2. Run the built `sigil --version` and `sigil doctor` smoke checks.
3. Generate GitHub artifact provenance attestations for each archive.
4. Upload archives and SHA-256 checksum files as workflow artifacts.
5. Generate release notes from Conventional Commit subjects.
6. Render a Homebrew tap formula from the macOS archive URL and checksum.
7. Generate npm package tarballs from the release archives.
8. Publish a GitHub release with archives, checksum files, `checksums.txt`,
   `sigil-ai.rb`, npm package tarballs, and generated notes. Tags with a
   prerelease suffix, such as `v0.0.1-alpha.1`, are published as GitHub
   prereleases.
9. Update the `JimmyDaddy/homebrew-sigil` tap from the generated `sigil-ai.rb`
   asset and verify the tap points at the same release tag.

GitHub artifact attestations require `id-token: write`, `contents: read`, and
`attestations: write` permissions on the build job. The publish job only needs
`contents: write`.

## Assets

Each release should contain:

- `sigil-<version>-<target>.tar.gz`
- `sigil-<version>-<target>.tar.gz.sha256`
- `checksums.txt`
- `sigil-ai.rb` with arm64 and Intel macOS archive URLs when both macOS artifacts are available
- `sigil-ai-sigil-<version>.tgz`
- `sigil-ai-sigil-<platform>-<version>.tgz` for each supported npm platform package

Each tar archive should include the `sigil` binary, README files, `assets/logo/*`, and installation docs so repository-relative README image links keep working after extraction.

The generated `sigil-ai.rb` is the source of truth for the
`JimmyDaddy/homebrew-sigil` tap update. After the GitHub release succeeds, copy
that asset into `Formula/sigil-ai.rb` in the tap repository, run `ruby -c`, commit
the update, and push it before announcing the Homebrew path as current.

```bash
tmp_formula_dir="$(mktemp -d)"
gh release download v0.0.1-alpha.1 \
  --repo JimmyDaddy/sigil \
  --dir "${tmp_formula_dir}" \
  --pattern sigil-ai.rb

cd /path/to/homebrew-sigil
cp "${tmp_formula_dir}/sigil-ai.rb" Formula/sigil-ai.rb
ruby -c Formula/sigil-ai.rb
git diff -- Formula/sigil-ai.rb
git add Formula/sigil-ai.rb
git commit -m "chore: update sigil-ai to 0.0.1-alpha.1"
git push origin main
```

Verify the pushed tap formula references the same tag and version:

```bash
gh api repos/JimmyDaddy/homebrew-sigil/contents/Formula/sigil-ai.rb \
  --jq .content | base64 --decode | grep -E '0\.0\.1-alpha\.1|v0\.0\.1-alpha\.1'
```

This cross-repository tap sync is currently a required maintainer step. To
automate it inside `.github/workflows/release.yml`, use a fine-scoped GitHub App
token or PAT secret with `contents:write` on `JimmyDaddy/homebrew-sigil`; the
default `GITHUB_TOKEN` for this repository must not be assumed to have
cross-repository write permission.

The npm package tarballs are generated from the same release archives:

```bash
scripts/prepare-npm-packages.sh \
  --version 0.0.1-alpha.1 \
  --dist-dir dist \
  --out-dir dist/npm-packages \
  --pack-destination dist
```

The root npm package is `@sigil-ai/sigil`; platform-specific optional packages
carry the actual binaries. Publish the platform packages first, then publish the
root package:

```bash
npm publish dist/npm-packages/sigil-darwin-arm64 --access public --tag alpha
npm publish dist/npm-packages/sigil-darwin-x64 --access public --tag alpha
npm publish dist/npm-packages/sigil-linux-x64 --access public --tag alpha
npm publish dist/npm-packages/sigil-win32-x64 --access public --tag alpha
npm publish dist/npm-packages/sigil --access public --tag alpha
```

Prefer npm trusted publishing or provenance-capable CI for registry publication.
If a platform archive is not present, do not list or publish that optional
package for the release.

For the first published prerelease of a package, npm can keep `latest` pointing
at the only available version even when the package is published with
`--tag alpha`; the registry rejects removing `latest` when no alternate version
exists. User-facing install docs should still prefer `@alpha` for prereleases.

Cargo distribution for the first release uses the Git tag:

```bash
cargo install --git https://github.com/JimmyDaddy/sigil --tag v0.0.1-alpha.1 --locked sigil
```

Do not publish this workspace to crates.io as `sigil`; that crate name is already
owned by another package. A future crates.io release needs a separate package
name decision while keeping the installed binary named `sigil`.

## Release Notes

Release notes are generated by:

```bash
scripts/generate-release-notes.sh v0.0.1-alpha.1
```

The script groups Conventional Commit subjects into:

- Features
- Fixes
- Documentation
- Maintenance
- Other changes

## Local Checks

Before pushing a release tag, run:

```bash
cargo fmt --all --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
scripts/build-release-archive.sh
scripts/render-homebrew-formula.sh --version 0.0.1-alpha.1 --url https://example.invalid/sigil.tar.gz --sha256 0000000000000000000000000000000000000000000000000000000000000000 --output /tmp/sigil-ai.rb
scripts/generate-release-notes.sh HEAD >/tmp/sigil-release-notes.md
```

If the release workflow fails after publishing partial artifacts, delete the
draft or failed release before retrying the same tag.
