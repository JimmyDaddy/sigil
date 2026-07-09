# Sigil Logo Assets

These logo files are source-controlled for repository documentation, release metadata, package listings, and social previews.

| File | Size | Recommended use |
| --- | ---: | --- |
| `sigil-full.png` | 1094 x 545 | Primary transparent logo with mark and wordmark for legacy PNG surfaces. |
| `sigil-full-flat.svg` | 1094 x 545 | Vector primary transparent logo for the website hero and docs hero. |
| `sigil-full-flat-2x.png` | 2188 x 1090 | 2x raster fallback for high-DPI `sigil-full-flat.svg`. |
| `sigil-full-on-white.png` | 1094 x 545 | Full logo on an opaque white canvas for GitHub README surfaces and other dark-mode-sensitive renderers. |
| `sigil-mark-square-1024.png` | 1024 x 1024 | Square app, package, or social preview surfaces. |
| `sigil-mark-transparent.png` | 450 x 527 | Standalone mark on controlled backgrounds that preserve alpha. |
| `sigil-mark-on-white.png` | 450 x 450 | Standalone mark for surfaces that need a white canvas. |
| `sigil-wordmark-transparent.png` | 619 x 314 | Wordmark-only placement on controlled backgrounds that preserve alpha. |
| `sigil-wordmark-transparent.svg` | 619 x 314 | Vector wordmark-only placement for compact controlled backgrounds. |
| `sigil-wordmark-header.png` | 527 x 226 | Legacy PNG wordmark derived from transparent wordmark variants. |
| `sigil-wordmark-header.svg` | 527 x 226 | Current vector wordmark for website headers. |
| `sigil-wordmark-header-2x.png` | 1054 x 452 | 2x raster fallback for `sigil-wordmark-header.svg` on high-DPI displays. |
| `sigil-wordmark-transparent-2x.png` | 1238 x 628 | 2x raster transparent wordmark for future high-DPI uses. |
| `sigil-wordmark-on-white.png` | 619 x 314 | Wordmark-only placement for surfaces that need a white canvas. |

Regenerate `sigil-full.png`, `sigil-full-on-white.png`, and `sigil-wordmark-header.png` with `node scripts/generate-full-logo.mjs` after updating the transparent mark or wordmark assets.
Prefer repository-relative paths such as `assets/logo/sigil-full.png` in docs so links render correctly in GitHub and in release archives.
