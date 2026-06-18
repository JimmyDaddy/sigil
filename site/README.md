# Sigil GitHub Pages Site

This directory contains the zero-dependency static site published to GitHub Pages.

The Pages workflow stages this directory, copies `assets/logo/*.png` and docs examples into the published artifact, generates HTML pages from `docs/en/*.md` and `docs/zh-CN/*.md`, checks the required files, and deploys it with GitHub's Pages actions.

The site intentionally stays static and small. The homepage introduces Sigil, while `docs/` and `zh-CN/docs/` provide stable documentation hubs. Build output also includes generated pages such as `docs/quickstart/` and `zh-CN/docs/quickstart/`.

Expected project site URL after Pages is enabled:

```text
https://jimmydaddy.github.io/sigil/
```

Repository setting required once:

1. Open `Settings -> Pages`.
2. Set `Build and deployment -> Source` to `GitHub Actions`.
3. Push to `main` or run the `Pages` workflow manually.

Local preview:

```bash
scripts/preview-pages.sh
```

Regenerate TUI renderer captures after meaningful TUI layout changes:

```bash
scripts/generate-tui-screenshots.sh
```

Static artifact check:

```bash
scripts/check-pages-site.sh
```

The check runs Markdown link, bilingual mirror, command metadata, Pages artifact, sitemap, SEO metadata, and local-only URL checks.
