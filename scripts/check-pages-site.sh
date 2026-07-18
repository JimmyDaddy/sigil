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

stage_root="$(mktemp -d)"
trap 'rm -rf "${stage_root}"' EXIT
stage_dir="${stage_root}/public"

scripts/check-docs.sh >/dev/null
scripts/build-pages-site.sh "${stage_dir}" >/dev/null
ruby scripts/check-site-structure.rb "${stage_dir}" >/dev/null
node scripts/check-site-search-ranking.js "${stage_dir}/search.json" dev/docs/public-documentation-content-policy.json >/dev/null
scripts/check-site-metadata.rb "${stage_dir}" >/dev/null
scripts/check-site-accessibility.rb "${stage_dir}" >/dev/null
scripts/test-docs-table-render.rb >/dev/null
scripts/check-site-viewport.rb "${stage_dir}" >/dev/null
scripts/check-site-artifact-links.rb "${stage_dir}" >/dev/null
scripts/check-site-repo-links.rb "${stage_dir}" >/dev/null

if grep -R -n -E 'public-doc-(role|topic|cta)' "${stage_dir}/docs" "${stage_dir}/zh-CN/docs"; then
  echo "public documentation metadata leaked into rendered pages" >&2
  exit 1
fi

required_files=(
  "index.html"
  "docs/index.html"
  "docs/quickstart/index.html"
  "docs/safety/index.html"
  "docs/permissions-and-sandbox/index.html"
  "docs/appearance/index.html"
  "docs/advanced-configuration/index.html"
  "docs/configuration-reference/index.html"
  "docs/providers/index.html"
  "docs/provider-deepseek/index.html"
  "docs/provider-openai-compatible/index.html"
  "docs/provider-openai-responses/index.html"
  "docs/provider-anthropic/index.html"
  "docs/provider-gemini/index.html"
  "docs/privacy/index.html"
  "docs/status/index.html"
  "zh-CN/index.html"
  "zh-CN/docs/index.html"
  "zh-CN/docs/quickstart/index.html"
  "zh-CN/docs/safety/index.html"
  "zh-CN/docs/permissions-and-sandbox/index.html"
  "zh-CN/docs/appearance/index.html"
  "zh-CN/docs/advanced-configuration/index.html"
  "zh-CN/docs/configuration-reference/index.html"
  "zh-CN/docs/providers/index.html"
  "zh-CN/docs/provider-deepseek/index.html"
  "zh-CN/docs/provider-openai-compatible/index.html"
  "zh-CN/docs/provider-openai-responses/index.html"
  "zh-CN/docs/provider-anthropic/index.html"
  "zh-CN/docs/provider-gemini/index.html"
  "zh-CN/docs/privacy/index.html"
  "zh-CN/docs/status/index.html"
  "404.html"
  "CNAME"
  ".nojekyll"
  "robots.txt"
  "sitemap.xml"
  "search.json"
  "assets/site.css"
  "assets/site.js"
  "assets/code.js"
  "assets/search.js"
  "assets/search-ranking.js"
  "assets/logo/sigil-lockup.svg"
  "assets/logo/sigil-lockup-dark-mode.svg"
  "assets/logo/sigil-lockup.png"
  "assets/logo/sigil-lockup-2x.png"
  "assets/logo/sigil-mark.svg"
  "assets/logo/sigil-mark-dark-mode.svg"
  "assets/logo/sigil-mark-micro.svg"
  "assets/logo/sigil-mark-micro-dark-mode.svg"
  "assets/logo/sigil-mark.png"
  "assets/logo/sigil-mark-2x.png"
  "assets/logo/sigil-wordmark.svg"
  "assets/logo/sigil-wordmark-dark-mode.svg"
  "assets/logo/sigil-wordmark.png"
  "assets/logo/sigil-wordmark-2x.png"
  "assets/logo/sigil-full-staff-glow.svg"
  "assets/logo/sigil-full-staff-glow-dark-mode.svg"
  "assets/logo/sigil-full-staff-glow-2x.png"
  "assets/logo/sigil-full-staff-glow.png"
  "assets/logo/sigil-mark-staff-glow.svg"
  "assets/logo/sigil-mark-staff-glow.png"
  "assets/logo/sigil-mark-staff-glow-2x.png"
  "assets/logo/sigil-mark-staff-glow-watermark.svg"
  "assets/logo/sigil-mark-staff-glow-watermark-4x.png"
  "assets/logo/sigil-wordmark-header.svg"
  "assets/logo/sigil-wordmark-header-2x.png"
  "assets/social/sigil-social-preview.svg"
  "assets/social/sigil-social-preview.png"
  "assets/demo/sigil-45-second-demo.mp4"
  "assets/demo/sigil-45-second-demo.webm"
  "assets/demo/sigil-45-second-demo-poster.png"
  "assets/demo/sigil-45-second-demo.en.vtt"
  "assets/demo/sigil-45-second-demo.zh-CN.vtt"
  "assets/screenshots/tui-session.svg"
  "assets/screenshots/approval-review.svg"
  "assets/screenshots/config-panel.svg"
  "assets/screenshots/verification-card.svg"
  "assets/screenshots/checkpoint-restore.svg"
  "assets/screenshots/compaction-preview.svg"
  "examples/config/index.html"
  "examples/config/deepseek-basic.toml"
  "examples/config/openai-compatible.toml"
  "examples/config/anthropic.toml"
  "examples/config/gemini.toml"
)

source_docs=(
  "docs/en/README.md"
  "docs/en/quickstart.md"
  "docs/en/installation.md"
  "docs/en/visual-tour.md"
  "docs/en/workflows.md"
  "docs/en/cookbook.md"
  "docs/en/user-guide.md"
  "docs/en/safety.md"
  "docs/en/configuration.md"
  "docs/en/permissions-and-sandbox.md"
  "docs/en/appearance.md"
  "docs/en/advanced-configuration.md"
  "docs/en/configuration-reference.md"
  "docs/en/providers.md"
  "docs/en/provider-deepseek.md"
  "docs/en/provider-openai-compatible.md"
  "docs/en/provider-openai-responses.md"
  "docs/en/provider-anthropic.md"
  "docs/en/provider-gemini.md"
  "docs/en/privacy.md"
  "docs/en/troubleshooting.md"
  "docs/en/reference.md"
  "docs/en/mcp.md"
  "docs/en/terminal-compatibility.md"
  "docs/en/status.md"
  "docs/en/changelog.md"
  "docs/zh-CN/README.md"
  "docs/zh-CN/quickstart.md"
  "docs/zh-CN/installation.md"
  "docs/zh-CN/visual-tour.md"
  "docs/zh-CN/workflows.md"
  "docs/zh-CN/cookbook.md"
  "docs/zh-CN/user-guide.md"
  "docs/zh-CN/safety.md"
  "docs/zh-CN/configuration.md"
  "docs/zh-CN/permissions-and-sandbox.md"
  "docs/zh-CN/appearance.md"
  "docs/zh-CN/advanced-configuration.md"
  "docs/zh-CN/configuration-reference.md"
  "docs/zh-CN/providers.md"
  "docs/zh-CN/provider-deepseek.md"
  "docs/zh-CN/provider-openai-compatible.md"
  "docs/zh-CN/provider-openai-responses.md"
  "docs/zh-CN/provider-anthropic.md"
  "docs/zh-CN/provider-gemini.md"
  "docs/zh-CN/privacy.md"
  "docs/zh-CN/troubleshooting.md"
  "docs/zh-CN/reference.md"
  "docs/zh-CN/mcp.md"
  "docs/zh-CN/terminal-compatibility.md"
  "docs/zh-CN/status.md"
  "docs/zh-CN/changelog.md"
)

for file in "${required_files[@]}"; do
  if [[ ! -f "${stage_dir}/${file}" ]]; then
    echo "missing Pages artifact file: ${file}" >&2
    exit 1
  fi
done

social_preview="${stage_dir}/assets/social/sigil-social-preview.png"
ruby -e '
  data = File.binread(ARGV.fetch(0), 24)
  signature = "\x89PNG\r\n\x1A\n".b
  abort "social preview is not a PNG" unless data.start_with?(signature)
  width, height = data.byteslice(16, 8).unpack("NN")
  abort "social preview must be 1280x640, found #{width}x#{height}" unless [width, height] == [1280, 640]
' "${social_preview}"
if [[ "$(wc -c < "${social_preview}")" -ge 1048576 ]]; then
  echo "social preview must remain below 1 MiB for GitHub upload" >&2
  exit 1
fi
if grep -q '<image' "${stage_dir}/assets/social/sigil-social-preview.svg"; then
  echo "social preview SVG must remain self-contained" >&2
  exit 1
fi

ruby -e '
  png = File.binread(ARGV.fetch(0), 24)
  abort "demo poster is not a PNG" unless png.start_with?("\x89PNG\r\n\x1A\n".b)
  width, height = png.byteslice(16, 8).unpack("NN")
  abort "demo poster must be 1920x1080, found #{width}x#{height}" unless [width, height] == [1920, 1080]

  mp4 = File.binread(ARGV.fetch(1), 12)
  abort "demo MP4 is missing an ftyp box" unless mp4.byteslice(4, 4) == "ftyp"

  webm = File.binread(ARGV.fetch(2), 4)
  abort "demo WebM has an invalid EBML header" unless webm == "\x1A\x45\xDF\xA3".b

  ARGV.drop(3).each do |caption|
    abort "demo caption must start with WEBVTT: #{caption}" unless File.read(caption, 6) == "WEBVTT"
  end
' \
  "${stage_dir}/assets/demo/sigil-45-second-demo-poster.png" \
  "${stage_dir}/assets/demo/sigil-45-second-demo.mp4" \
  "${stage_dir}/assets/demo/sigil-45-second-demo.webm" \
  "${stage_dir}/assets/demo/sigil-45-second-demo.en.vtt" \
  "${stage_dir}/assets/demo/sigil-45-second-demo.zh-CN.vtt"

for file in "${source_docs[@]}"; do
  if [[ ! -f "${repo_root}/${file}" ]]; then
    echo "missing source documentation file: ${file}" >&2
    exit 1
  fi
done

grep -q 'href="zh-CN/"' "${stage_dir}/index.html"
grep -q 'href="../"' "${stage_dir}/zh-CN/index.html"
grep -q 'href="docs/#quickstart"' "${stage_dir}/index.html"
grep -q 'href="docs/#quickstart"' "${stage_dir}/zh-CN/index.html"
grep -q 'href="quickstart/"' "${stage_dir}/docs/index.html"
grep -q 'href="user-guide/"' "${stage_dir}/docs/index.html"
grep -q 'href="workflows/"' "${stage_dir}/docs/index.html"
grep -q 'href="configuration/"' "${stage_dir}/docs/index.html"
grep -q 'href="safety/"' "${stage_dir}/docs/index.html"
grep -q 'href="providers/"' "${stage_dir}/docs/index.html"
grep -q 'href="troubleshooting/"' "${stage_dir}/docs/index.html"
grep -q 'href="reference/"' "${stage_dir}/docs/index.html"
grep -q 'href="quickstart/"' "${stage_dir}/zh-CN/docs/index.html"
grep -q 'href="user-guide/"' "${stage_dir}/zh-CN/docs/index.html"
grep -q 'href="workflows/"' "${stage_dir}/zh-CN/docs/index.html"
grep -q 'href="configuration/"' "${stage_dir}/zh-CN/docs/index.html"
grep -q 'href="safety/"' "${stage_dir}/zh-CN/docs/index.html"
grep -q 'href="providers/"' "${stage_dir}/zh-CN/docs/index.html"
grep -q 'href="troubleshooting/"' "${stage_dir}/zh-CN/docs/index.html"
grep -q 'href="reference/"' "${stage_dir}/zh-CN/docs/index.html"
grep -q 'href="../zh-CN/docs/"' "${stage_dir}/docs/index.html"
grep -q 'href="../../docs/"' "${stage_dir}/zh-CN/docs/index.html"
grep -q 'https://sigil.corerobin.com/' "${stage_dir}/sitemap.xml"
grep -q 'https://sigil.corerobin.com/docs/' "${stage_dir}/sitemap.xml"
grep -q 'https://sigil.corerobin.com/zh-CN/docs/' "${stage_dir}/sitemap.xml"
grep -q 'https://sigil.corerobin.com/docs/quickstart/' "${stage_dir}/sitemap.xml"
grep -q 'https://sigil.corerobin.com/zh-CN/docs/quickstart/' "${stage_dir}/sitemap.xml"
grep -q 'https://sigil.corerobin.com/docs/provider-deepseek/' "${stage_dir}/sitemap.xml"
grep -q 'https://sigil.corerobin.com/zh-CN/docs/provider-gemini/' "${stage_dir}/sitemap.xml"
grep -q 'Sitemap: https://sigil.corerobin.com/sitemap.xml' "${stage_dir}/robots.txt"
grep -qx 'sigil.corerobin.com' "${stage_dir}/CNAME"
grep -q 'property="og:image"' "${stage_dir}/index.html"
grep -q 'property="og:image"' "${stage_dir}/docs/index.html"
grep -q 'property="og:image"' "${stage_dir}/zh-CN/docs/index.html"
grep -q 'content="https://sigil.corerobin.com/assets/social/sigil-social-preview.png"' "${stage_dir}/index.html"
grep -q 'content="https://sigil.corerobin.com/assets/social/sigil-social-preview.png"' "${stage_dir}/docs/index.html"
grep -q 'content="https://sigil.corerobin.com/assets/social/sigil-social-preview.png"' "${stage_dir}/zh-CN/docs/index.html"
grep -q 'content="1280"' "${stage_dir}/index.html"
grep -q 'content="640"' "${stage_dir}/index.html"
if grep -Eq '<(changefreq|priority)>' "${stage_dir}/sitemap.xml"; then
  echo "sitemap contains metadata ignored by Google" >&2
  exit 1
fi
grep -q '<span class="brand-wordmark" aria-hidden="true"></span>' "${stage_dir}/index.html"
grep -q '<span class="brand-wordmark" aria-hidden="true"></span>' "${stage_dir}/docs/index.html"
grep -q '<span class="brand-wordmark" aria-hidden="true"></span>' "${stage_dir}/zh-CN/docs/index.html"
grep -q '<span class="brand-wordmark" aria-hidden="true"></span>' "${stage_dir}/docs/quickstart/index.html"
grep -q 'class="brand-mark" src="assets/logo/sigil-mark-staff-glow.svg"' "${stage_dir}/index.html"
grep -q 'class="brand-mark" src="../assets/logo/sigil-mark-staff-glow.svg"' "${stage_dir}/docs/index.html"
grep -q 'class="brand-mark" src="../../assets/logo/sigil-mark-staff-glow.svg"' "${stage_dir}/zh-CN/docs/index.html"
grep -q 'class="brand-mark" src="../../assets/logo/sigil-mark-staff-glow.svg"' "${stage_dir}/docs/quickstart/index.html"
grep -q 'data-theme-menu' "${stage_dir}/index.html"
grep -q 'data-theme-option="system"' "${stage_dir}/index.html"
grep -q 'data-theme-menu' "${stage_dir}/docs/index.html"
grep -q 'data-theme-menu' "${stage_dir}/zh-CN/docs/index.html"
grep -q 'data-theme-menu' "${stage_dir}/docs/quickstart/index.html"
grep -q 'src="assets/site.js"' "${stage_dir}/index.html"
grep -q 'src="assets/code.js"' "${stage_dir}/index.html"
grep -q 'id="demo"' "${stage_dir}/index.html"
grep -q 'poster="assets/demo/sigil-45-second-demo-poster.png"' "${stage_dir}/index.html"
grep -q 'src="assets/demo/sigil-45-second-demo.mp4"' "${stage_dir}/index.html"
grep -q 'id="demo"' "${stage_dir}/zh-CN/index.html"
grep -q 'poster="../assets/demo/sigil-45-second-demo-poster.png"' "${stage_dir}/zh-CN/index.html"
grep -q 'src="../assets/demo/sigil-45-second-demo.mp4"' "${stage_dir}/zh-CN/index.html"
grep -q 'src="../assets/site.js"' "${stage_dir}/docs/index.html"
grep -q 'src="../../assets/site.js"' "${stage_dir}/zh-CN/docs/index.html"
grep -q 'src="../../assets/site.js"' "${stage_dir}/docs/quickstart/index.html"
grep -q 'src="../../assets/code.js"' "${stage_dir}/docs/quickstart/index.html"
grep -q 'src="../../assets/search-ranking.js"' "${stage_dir}/docs/quickstart/index.html"
grep -q 'src="../assets/search-ranking.js"' "${stage_dir}/docs/index.html"
grep -q 'src="../../assets/search-ranking.js"' "${stage_dir}/zh-CN/docs/index.html"
if grep -R -n 'class="brand-mark" src="[^"]*sigil-mark-square-1024.png"' "${stage_dir}/index.html" "${stage_dir}/docs" "${stage_dir}/zh-CN"; then
  echo "square package icon leaked into Pages header brand mark" >&2
  exit 1
fi
if grep -R -n 'brand-wordmark-on-' "${stage_dir}/index.html" "${stage_dir}/docs" "${stage_dir}/zh-CN"; then
  echo "image-swapped wordmark leaked into Pages header" >&2
  exit 1
fi
if grep -R -n '<span>Sigil</span>' "${stage_dir}/index.html" "${stage_dir}/docs" "${stage_dir}/zh-CN"; then
  echo "text Sigil brand leaked into Pages header" >&2
  exit 1
fi
grep -q 'application/ld+json' "${stage_dir}/docs/quickstart/index.html"
grep -q 'data-index="../../search.json"' "${stage_dir}/docs/quickstart/index.html"
grep -q 'data-index="../../../search.json"' "${stage_dir}/zh-CN/docs/quickstart/index.html"
grep -q 'data-locale="en"' "${stage_dir}/docs/index.html"
grep -q 'data-locale="zh-CN"' "${stage_dir}/zh-CN/docs/index.html"
grep -q '"url": "docs/provider-deepseek/"' "${stage_dir}/search.json"
grep -q '"url": "zh-CN/docs/provider-gemini/"' "${stage_dir}/search.json"
grep -q '<img src="../../assets/screenshots/tui-session.svg"' "${stage_dir}/docs/visual-tour/index.html"
grep -q '<img src="../../../assets/screenshots/tui-session.svg"' "${stage_dir}/zh-CN/docs/visual-tour/index.html"
grep -q '<img src="../../assets/screenshots/verification-card.svg"' "${stage_dir}/docs/visual-tour/index.html"
grep -q '<img src="../../../assets/screenshots/checkpoint-restore.svg"' "${stage_dir}/zh-CN/docs/visual-tour/index.html"
grep -q 'Generated from Sigil TUI renderer' "${stage_dir}/assets/screenshots/tui-session.svg"
grep -q 'Generated from Sigil TUI renderer' "${stage_dir}/assets/screenshots/approval-review.svg"
grep -q 'Generated from Sigil TUI renderer' "${stage_dir}/assets/screenshots/config-panel.svg"
grep -q 'Generated from Sigil TUI renderer' "${stage_dir}/assets/screenshots/verification-card.svg"
grep -q 'Generated from Sigil TUI renderer' "${stage_dir}/assets/screenshots/checkpoint-restore.svg"
grep -q 'Generated from Sigil TUI renderer' "${stage_dir}/assets/screenshots/compaction-preview.svg"
grep -q 'name="twitter:card"' "${stage_dir}/index.html"

if grep -R -n -E 'host\.docker\.internal|127\.0\.0\.1|localhost' "${stage_dir}"; then
  echo "local-only URL leaked into Pages artifact" >&2
  exit 1
fi

echo "Pages site check passed"
