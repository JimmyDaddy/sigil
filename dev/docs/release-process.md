# Sigil Release Process

This document describes the maintainer release path. The user-facing install
entrypoint remains `sigil`; package manager wrappers must not introduce a new
product surface.

## Release Trigger

Create and push a version tag that matches the Cargo package version:

```bash
git tag -a v0.0.1-alpha.2 -m "Sigil 0.0.1-alpha.2"
git push origin v0.0.1-alpha.2
```

Before creating a tag, manually dispatch the `Release` workflow from the exact
candidate ref with `publish` left at its safe default of `false`. This build-only
mode runs all four platform archive, smoke, checksum, attestation, and artifact
upload steps, but it does not publish npm packages, create a GitHub Release, or
update the Homebrew tap.

Manual recovery of an already-created tag requires both `publish: true` and the
existing `v`-prefixed tag. The workflow checks out that tag for every build and
publish step. Treat this as a public release action, not as package preflight.

## Workflow

The release workflow is `.github/workflows/release.yml`. Tag pushes always use
publish mode; manual dispatch defaults to build-only mode.

1. Build native release archives on Ubuntu, macOS arm64, macOS Intel, and Windows runners.
2. Run the built `sigil --version` and `sigil doctor` smoke checks.
3. Generate GitHub artifact provenance attestations for each archive.
4. Upload archives and SHA-256 checksum files as workflow artifacts.
5. Generate release notes from Conventional Commit subjects.
6. Render a Homebrew tap formula from the macOS archive URL and checksum.
7. Generate npm package tarballs from the release archives.
8. Publish the platform-specific npm packages through npm trusted publishing,
   then publish the root `@sigil-ai/sigil` launcher package. Prereleases use the
   `alpha` dist-tag; reruns skip package versions already present in the registry.
9. Publish a GitHub release with archives, checksum files, `checksums.txt`,
   `sigil-ai.rb`, npm package tarballs, and generated notes. Tags with a
   prerelease suffix, such as `v0.0.1-alpha.2`, are published as GitHub
   prereleases.
10. Update the `JimmyDaddy/homebrew-sigil` tap from the generated `sigil-ai.rb`
   asset and verify the tap points at the same release tag.

GitHub artifact attestations require `id-token: write`, `contents: read`, and
`attestations: write` permissions on the build job. The publish job requires
`contents: write` for the GitHub release and `id-token: write` for npm trusted
publishing. It uses Node `22.22.0`, npm `11.18.0`, and no long-lived npm token.
The Homebrew sync job has read-only access to this repository and pushes to the
tap with the `HOMEBREW_TAP_DEPLOY_KEY` SSH deploy key, which is scoped to
`JimmyDaddy/homebrew-sigil` only.

## Assets

Each release should contain:

- `sigil-<version>-<target>.tar.gz`
- `sigil-<version>-<target>.tar.gz.sha256`
- `checksums.txt`
- `sigil-ai.rb` with arm64 and Intel macOS archive URLs when both macOS artifacts are available
- `sigil-ai-sigil-<version>.tgz`
- `sigil-ai-sigil-<platform>-<version>.tgz` for each supported npm platform package

Each tar archive should include the `sigil` binary, `LICENSE`, README files,
`assets/logo/*`, and installation docs so the license and repository-relative
README image links remain available after extraction.

The generated `sigil-ai.rb` is the source of truth for the
`JimmyDaddy/homebrew-sigil` tap update. After the GitHub release succeeds, the
separate `Sync Homebrew tap` job downloads that published asset, validates its
version and Ruby syntax, and commits it to `Formula/sigil-ai.rb`. Keeping this in
a separate job allows a failed tap push to be rerun without republishing npm
packages or recreating the GitHub release.

The repository secret `HOMEBREW_TAP_DEPLOY_KEY` must contain the private half of
a write-enabled deploy key registered only on `JimmyDaddy/homebrew-sigil`. Do
not reuse a maintainer PAT or the local GitHub CLI token for this job. The
default `GITHUB_TOKEN` remains limited to the `sigil` repository.

If the sync job needs manual recovery, download and validate the exact release
asset before committing it to the tap:

```bash
tmp_formula_dir="$(mktemp -d)"
gh release download v0.0.1-alpha.2 \
  --repo JimmyDaddy/sigil \
  --dir "${tmp_formula_dir}" \
  --pattern sigil-ai.rb

cd /path/to/homebrew-sigil
cp "${tmp_formula_dir}/sigil-ai.rb" Formula/sigil-ai.rb
ruby -c Formula/sigil-ai.rb
git diff -- Formula/sigil-ai.rb
git add Formula/sigil-ai.rb
git commit -m "chore: update sigil-ai to 0.0.1-alpha.2"
git push origin main
```

Verify the pushed tap formula references the same tag and version:

```bash
gh api repos/JimmyDaddy/homebrew-sigil/contents/Formula/sigil-ai.rb \
  --jq .content | base64 --decode | grep -E '0\.0\.1-alpha\.2|v0\.0\.1-alpha\.2'
```

The npm package tarballs are generated from the same release archives:

```bash
scripts/prepare-npm-packages.sh \
  --version 0.0.1-alpha.2 \
  --dist-dir dist \
  --out-dir dist/npm-packages \
  --pack-destination dist
```

The root npm package is `@sigil-ai/sigil`; platform-specific optional packages
carry the actual binaries. Every published package must configure the same npm
Trusted Publisher connection:

- provider: GitHub Actions
- organization or user: `JimmyDaddy`
- repository: `sigil`
- workflow filename: `release.yml`
- environment: unset
- allowed action: `npm publish`

The workflow calls `scripts/publish-npm-packages.sh`, which publishes platform
packages first and the root package last. It skips an exact package version that
already exists so the release job can resume safely after a partial registry
publish. To inspect the package order locally without registry access:

```bash
scripts/publish-npm-packages.sh \
  --version 0.0.1-alpha.2 \
  --packages-dir dist/npm-packages \
  --tag alpha \
  --dry-run
```

npm trusted publishing automatically creates provenance for public packages
published from this public repository. If a platform archive is not present, do
not list or publish that optional package for the release. Keep traditional
token publishing enabled until the first OIDC release succeeds; then restrict
publishing access and revoke obsolete automation tokens.

For the first published prerelease of a package, npm can keep `latest` pointing
at the only available version even when the package is published with
`--tag alpha`; the registry rejects removing `latest` when no alternate version
exists. User-facing install docs should still prefer `@alpha` for prereleases.

Cargo distribution for the first release uses the Git tag:

```bash
cargo install --git https://github.com/JimmyDaddy/sigil --tag v0.0.1-alpha.2 --locked sigil
```

Do not publish this workspace to crates.io as `sigil`; that crate name is already
owned by another package. A future crates.io release needs a separate package
name decision while keeping the installed binary named `sigil`.

## Release Notes

Release notes are generated by:

```bash
scripts/generate-release-notes.sh v0.0.1-alpha.2
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
scripts/render-homebrew-formula.sh --version 0.0.1-alpha.2 --url https://example.invalid/sigil.tar.gz --sha256 0000000000000000000000000000000000000000000000000000000000000000 --output /tmp/sigil-ai.rb
scripts/generate-release-notes.sh HEAD >/tmp/sigil-release-notes.md
```

If the release workflow fails after publishing partial artifacts, delete the
draft or failed release before retrying the same tag.
