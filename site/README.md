# Sigil GitHub Pages Site

This directory contains the zero-dependency source assets for the static site
published to GitHub Pages.

The Pages workflow stages this directory, copies `assets/logo/*.{png,svg}` and
docs examples into the published artifact, generates HTML pages from
`docs/en/*.md` and `docs/zh-CN/*.md`, checks the required files, and deploys it
with GitHub's Pages actions.

The site intentionally stays static and small. The homepage introduces Sigil,
while `docs/` and `zh-CN/docs/` provide stable documentation hubs. Build output
also includes generated pages such as `docs/quickstart/`,
`zh-CN/docs/quickstart/`, `search.json`, and `sitemap.xml`.

Do not edit generated staging artifacts directly. Regenerate the staged Pages
site with `scripts/build-pages-site.sh`; update the checked-in files here only
when changing source assets, hand-written hub pages, CSS, JavaScript, screenshots,
or static metadata such as `robots.txt`.

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

Regenerate all six TUI renderer captures after meaningful changes to the main session, approval, configuration, verification, checkpoint recovery, or compaction review surfaces:

```bash
scripts/generate-tui-screenshots.sh
```

Static artifact check:

```bash
scripts/check-pages-site.sh
```

The check runs Markdown link, bilingual mirror, command metadata, Pages artifact, repository blob link, sitemap, SEO metadata, and local-only URL checks.
