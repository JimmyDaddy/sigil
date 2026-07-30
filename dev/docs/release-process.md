# Sigil Release Process

This document describes the maintainer release path. The user-facing install
entrypoint remains `sigil`; package manager wrappers must not introduce a new
product surface.

## Release Trigger

Create and push a version tag that matches the Cargo package version:

```bash
git tag -a v0.0.1-beta.1 -m "Sigil 0.0.1-beta.1"
git push origin v0.0.1-beta.1
```

Before creating a tag, manually dispatch the `Release` workflow from the exact
candidate ref with `publish` left at its safe default of `false`. This build-only
mode runs all four platform archive, smoke, checksum, attestation, and artifact
upload steps, but it does not publish npm packages, create or change a GitHub
Release, or update the Homebrew tap.

Pushing the tag builds the exact source and creates or refreshes a **draft**
GitHub Release containing the TUI archives, npm tarballs, checksum files, and
Homebrew formula. It does not publish npm, make the release public, or update
Homebrew. Upload the locally signed/notarized Desktop assets to that draft
before the final publish dispatch.

Final publication requires a manual dispatch with `publish: true` and the
existing `v`-prefixed tag. The workflow checks out that tag, rebuilds and
revalidates the release assets, and for a beta requires both macOS Desktop
architectures. Native arm64 and Intel macOS jobs then download the completed
draft and re-check Developer ID authority, the exact Apple Team ID, Hardened
Runtime, stapler, Gatekeeper, updater signature, tag version, and tagged Git
commit for both the DMG app and updater archive app. Only those jobs can unblock
the separate public-release job. Treat this as a public release action, not as
package preflight.

The workflow serializes every run for the same tag. Final publication is also
serialized per channel and compares the candidate with all public GitHub
releases and the current npm dist-tag before changing public state. An older
tag therefore cannot roll a channel backward.

The optional `orchestration_eval_manifest_url` and
`orchestration_eval_manifest_sha256` inputs are a paired release-owner control.
Use them only for a report produced by the exact tagged commit after the
RFC-0053 deterministic, PTY, chaos, and real-model gates pass. Every native
candidate binary validates the downloaded report before deriving a path-free
`sigil-orchestration-rollout-v1.json` sidecar. Invalid, stale, unqualified, or
different-build reports fail the archive build. Leaving both inputs empty
produces a conservative release without the sidecar.

## Workflow

The release workflow is `.github/workflows/release.yml`. It deliberately uses
two phases so repositories with immutable GitHub Releases enabled never need to
append Desktop assets after publication.

1. Build native release archives on Ubuntu, macOS arm64, macOS Intel, and Windows runners.
2. Run the built `sigil --version` and `sigil doctor` smoke checks.
3. Generate GitHub artifact provenance attestations for each archive.
4. Upload archives and SHA-256 checksum files as workflow artifacts.
5. Generate release notes from Conventional Commit subjects.
6. Render a Homebrew tap formula from the macOS archive URL and checksum.
7. Generate npm package tarballs from the release archives.
8. On tag push, create or update a draft GitHub Release with archives, checksum
   files, `checksums.txt`, `sigil-ai.rb`, npm package tarballs, and generated
   notes. A tag push never publishes the draft.
9. Upload the signed Desktop DMGs, checksums, updater archives, and updater
   signatures to the same draft.
10. On explicit publish, require the beta Desktop asset matrix, verify the DMG
   and updater archive checksums, cryptographically verify both updater
   signatures against the public key embedded in the tagged Tauri config, run a
   swapped-signature negative control, and generate `latest.json`.
11. On native arm64 and Intel macOS runners, download the exact draft assets,
   safely validate and extract the updater archive, and independently verify
   Developer ID, Team ID, Hardened Runtime, version, commit, stapler, and
   Gatekeeper evidence for both the DMG and updater app.
12. Prove through GitHub's immutable-releases repository API that release
   immutability is enabled, then make the completed draft public. Prerelease
   suffixes stay marked as GitHub prereleases; no release assets are appended
   afterward.
13. Publish npm only after the release is accessible. `-alpha.*` uses the
   `alpha` dist-tag, `-beta.*` uses `beta`, and unknown prerelease suffixes fail.
   Registry reads and SemVer comparison fail closed. An exact package-version
   retry is skipped only when the requested dist-tag already points to that
   exact version.
14. Copy the immutable release `latest.json` into the full Pages artifact at
   `/updates/beta/latest.json` and deploy that artifact as the Desktop updater
   endpoint. Normal `main` Pages deployments resolve the newest published beta
   across every API page, select the SemVer-maximum immutable published beta,
   validate its exact updater asset names and GitHub download URLs, and copy its
   immutable manifest too. The release deployment also compares the current
   public manifest before upload. API or publication ordering therefore cannot
   erase or roll back the update endpoint.
15. For beta releases, update the single `JimmyDaddy/homebrew-sigil` tap from
   the generated `sigil-ai.rb` asset only after a SemVer monotonicity check and
   verify the tap points at the same release tag. Alpha remains an npm/GitHub
   channel and does not compete for the single Homebrew formula.

GitHub artifact attestations require `id-token: write`, `contents: read`, and
`attestations: write` permissions on the build job. The publish job requires
`contents: write` for the GitHub release and `id-token: write` for npm trusted
publishing. It uses Node `22.22.0`, npm `11.18.0`, and no long-lived npm token.
The immutable-releases preflight uses `SIGIL_RELEASE_ADMIN_TOKEN` when present,
falling back to `GITHUB_TOKEN` only if that token can read repository
administration state. Configure the secret with repository Administration
read-only access when the default token cannot call
`GET /repos/JimmyDaddy/sigil/immutable-releases`; an unreadable, missing, or
disabled policy blocks publication.
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
- `Sigil_<version>_aarch64-apple-darwin.dmg`
- `Sigil_<version>_aarch64-apple-darwin.dmg.sha256`
- `Sigil_<version>_x86_64-apple-darwin.dmg`
- `Sigil_<version>_x86_64-apple-darwin.dmg.sha256`
- `Sigil_<version>_<mac-target>.app.tar.gz` plus matching `.sha256` and `.sig` for both macOS targets
- `latest.json`, generated only after the signed updater archives are present

Each tar archive should include the `sigil` binary, `LICENSE`, README files,
`assets/logo/*`, `site/assets/screenshots/tui-session.svg`, and installation docs
so the license and repository-relative README image links remain available after
extraction.

Qualified-route releases also include
`sigil-orchestration-rollout-v1.json` beside the binary. The npm platform package
copies it into the same `bin` directory, and the Homebrew formula installs it
beside `sigil`. The root npm launcher does not interpret the file. Missing
sidecars are valid and retain `manual + explicit_request_only`; packaging must
never synthesize a sidecar without a report accepted by the exact binary.

The generated `sigil-ai.rb` is the source of truth for the
`JimmyDaddy/homebrew-sigil` tap update. After the draft becomes public, the
separate `Sync Homebrew tap` job downloads that published asset, validates its
version and Ruby syntax, and commits it to `Formula/sigil-ai.rb`. Keeping this in
a separate job allows a failed tap push to be rerun without republishing npm
packages or recreating the GitHub release. The tap tracks the beta channel;
published distribution smoke is likewise fixed to beta so it never compares an
alpha npm package with a beta Homebrew formula.

The repository secret `HOMEBREW_TAP_DEPLOY_KEY` must contain the private half of
a write-enabled deploy key registered only on `JimmyDaddy/homebrew-sigil`. Do
not reuse a maintainer PAT or the local GitHub CLI token for this job. The
default `GITHUB_TOKEN` remains limited to the `sigil` repository.

If the sync job needs manual recovery, download and validate the exact release
asset before committing it to the tap:

```bash
tmp_formula_dir="$(mktemp -d)"
gh release download v0.0.1-beta.1 \
  --repo JimmyDaddy/sigil \
  --dir "${tmp_formula_dir}" \
  --pattern sigil-ai.rb

cd /path/to/homebrew-sigil
cp "${tmp_formula_dir}/sigil-ai.rb" Formula/sigil-ai.rb
ruby -c Formula/sigil-ai.rb
git diff -- Formula/sigil-ai.rb
git add Formula/sigil-ai.rb
git commit -m "chore: update sigil-ai to 0.0.1-beta.1"
git push origin main
```

Verify the pushed tap formula references the same tag and version:

```bash
gh api repos/JimmyDaddy/homebrew-sigil/contents/Formula/sigil-ai.rb \
  --jq .content | base64 --decode | grep -E '0\.0\.1-beta\.1|v0\.0\.1-beta\.1'
```

The npm package tarballs are generated from the same release archives:

```bash
scripts/prepare-npm-packages.sh \
  --version 0.0.1-beta.1 \
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

The workflow calls `scripts/publish-npm-packages.sh`, which derives `alpha` or
`beta` from the prerelease suffix and publishes platform
packages first and the root package last. It skips an exact package version that
already exists only after proving the selected dist-tag already equals that
version, so a retry cannot silently preserve the wrong desired state or move a
newer tag backward. To inspect the package order locally without registry access:

```bash
scripts/publish-npm-packages.sh \
  --version 0.0.1-beta.1 \
  --packages-dir dist/npm-packages \
  --tag beta \
  --dry-run
```

npm trusted publishing automatically creates provenance for public packages
published from this public repository. If a platform archive is not present, do
not list or publish that optional package for the release. Keep traditional
token publishing enabled until the first OIDC release succeeds; then restrict
publishing access and revoke obsolete automation tokens.

For the first published prerelease of a package, npm can keep `latest` pointing
at the only available version even when the package is published with an
explicit prerelease tag; the registry rejects removing `latest` when no
alternate version exists. User-facing install docs should still use the channel
matching the release suffix.

Cargo distribution for the first release uses the Git tag:

```bash
cargo install --git https://github.com/JimmyDaddy/sigil --tag v0.0.1-beta.1 --locked sigil
```

Do not publish this workspace to crates.io as `sigil`; that crate name is already
owned by another package. A future crates.io release needs a separate package
name decision while keeping the installed binary named `sigil`.

## Release Notes

Release notes are generated by:

```bash
scripts/generate-release-notes.sh v0.0.1-beta.1
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
scripts/render-homebrew-formula.sh --version 0.0.1-beta.1 --url https://example.invalid/sigil.tar.gz --sha256 0000000000000000000000000000000000000000000000000000000000000000 --output /tmp/sigil-ai.rb
scripts/generate-release-notes.sh HEAD >/tmp/sigil-release-notes.md
```

After an exact candidate has produced a qualified report, exercise the
sidecar-bearing archive path separately:

```bash
scripts/build-release-archive.sh \
  --orchestration-eval-manifest /absolute/path/to/orchestration/manifest.json
```

The command must fail if the report targets a different commit or build. Do not
copy a report-derived sidecar into another archive manually.

If staging fails, keep the draft and rerun the same tag after fixing or adding
its assets. If npm, Pages, or Homebrew fails after the draft becomes public,
rerun the explicit publish dispatch; exact npm package versions are skipped
safely and the immutable `latest.json` is copied into a fresh full Pages
artifact. Never publish a draft until the Desktop matrix is complete, because
immutable GitHub Releases cannot accept missing assets afterward.
