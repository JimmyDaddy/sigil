# Sigil Logo Assets

These PNG files are source-controlled for repository documentation, release metadata, package listings, and social previews.

| File | Size | Recommended use |
| --- | ---: | --- |
| `sigil-full.png` | 1094 x 545 | Primary transparent logo with mark and wordmark for README, release pages, and the website hero. |
| `sigil-full-on-white.png` | 1094 x 545 | Full logo on an opaque white canvas for GitHub README surfaces and other dark-mode-sensitive renderers. |
| `sigil-mark-square-1024.png` | 1024 x 1024 | Square app, package, or social preview surfaces. |
| `sigil-mark-transparent.png` | 450 x 527 | Standalone mark on controlled backgrounds that preserve alpha. |
| `sigil-mark-on-white.png` | 450 x 450 | Standalone mark for surfaces that need a white canvas. |
| `sigil-wordmark-transparent.png` | 619 x 314 | Wordmark-only placement on controlled backgrounds that preserve alpha. |
| `sigil-wordmark-header.png` | 527 x 226 | Tight transparent wordmark derived from `sigil-wordmark-transparent.png` for compact website headers. |
| `sigil-wordmark-on-white.png` | 619 x 314 | Wordmark-only placement for surfaces that need a white canvas. |

Regenerate `sigil-full.png`, `sigil-full-on-white.png`, and `sigil-wordmark-header.png` with `node scripts/generate-full-logo.mjs` after updating the transparent mark or wordmark assets.
Prefer repository-relative paths such as `assets/logo/sigil-full.png` in docs so links render correctly in GitHub and in release archives.
