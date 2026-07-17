#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "${repo_root}"

ruby_compat="${repo_root}/scripts/ruby-compat.rb"
if [[ -n "${RUBYOPT:-}" ]]; then
  export RUBYOPT="-r${ruby_compat} ${RUBYOPT}"
else
  export RUBYOPT="-r${ruby_compat}"
fi

scripts/check-docs-links.rb
scripts/check-docs-mirror.rb
scripts/check-docs-command-metadata.rb
ruby scripts/check-public-doc-content.rb
ruby scripts/test-public-doc-content.rb
ruby scripts/check-public-doc-parity.rb

metrics_file="$(mktemp)"
trap 'rm -f "${metrics_file}"' EXIT
ruby scripts/public-doc-metrics.rb --output "${metrics_file}"
ruby -rjson -e 'payload = JSON.parse(File.read(ARGV.fetch(0))); abort "invalid public metrics schema" unless payload["schema_version"] == 1' "${metrics_file}"

echo "docs checks passed"
