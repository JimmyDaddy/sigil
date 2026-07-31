#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
workflow="${repo_root}/.github/workflows/published-distribution-smoke.yml"

test -s "${workflow}"
ruby -e 'require "yaml"; YAML.safe_load(File.read(ARGV.fetch(0)), aliases: true)' "${workflow}"

required_lines=(
  "  workflow_dispatch:"
  "  release:"
  "    types: [published]"
  "  repository_dispatch:"
  "    types: [sigil_published_distribution]"
  "  schedule:"
  "  attestations: read"
  "  contents: read"
  "  RELEASE_CHANNEL: beta"
  "      available: \${{ steps.release.outputs.available }}"
  "                printf 'available=false\\n' >> \"\${GITHUB_OUTPUT}\""
  "  npm-install:"
  "  github-release:"
  "  homebrew-install:"
  "          ref: \${{ needs.resolve-release.outputs.tag }}"
  "            scripts/verify-desktop-update-signature.sh \\"
  "              \"repos/\${GITHUB_REPOSITORY}/releases/tags/\${tag}\""
  "          if [[ \"\$(jq -r '.immutable // false' <<<\"\${release}\")\" != \"true\" ]]; then"
  "          npm exec -- sigil doctor --output json > doctor.json"
  "              sleep 30"
  "            --retry 20 --retry-delay 30 --retry-all-errors \\"
  "      - name: Wait for the public tap to converge"
  "            gh attestation verify \"\${archive}\" --repo \"\${GITHUB_REPOSITORY}\""
  "          node scripts/release-candidate.mjs verify \\"
  "          brew install JimmyDaddy/sigil/sigil-ai"
  "          sigil doctor --output json > doctor.json"
)
for line in "${required_lines[@]}"; do
  grep -Fqx "${line}" "${workflow}"
done

for forbidden in \
  "  pull_request:" \
  "  push:" \
  "npm publish" \
  "gh release create" \
  "git push" \
  "dist-tag add" \
  "inputs.channel" \
  "RELEASE_CHANNEL: alpha"; do
  if grep -Fq "${forbidden}" "${workflow}"; then
    echo "published distribution smoke contains forbidden mutation: ${forbidden}" >&2
    exit 1
  fi
done

if command -v actionlint >/dev/null 2>&1; then
  actionlint "${workflow}"
fi

echo "published distribution smoke workflow tests passed"
