# Sigil Logo Assets

The primary identity keeps the original broken hexagon and slab wordmark, while giving each symbol a stable role:

- **Ink** (`#242932` / `#EEF1F3`) forms the container and core letterforms.
- **Staff** (`#16BDB8` / `#5EFFF8`) forms the central staff and the second `i`.
- **Terminal** (`#C85B4B` / `#FF8A78`) forms the `>_` prompt in the mark and the `g` counter.

The flat vectors are the source of truth. Glow and shadow are presentation effects for large hero placements, not part of the logo geometry.

## Canonical assets

| File | Canvas | Recommended use |
| --- | ---: | --- |
| `sigil-mark.svg` | 64 × 64 | Default standalone mark on light backgrounds. |
| `sigil-mark-dark-mode.svg` | 64 × 64 | Standalone mark on dark backgrounds. |
| `sigil-mark-micro.svg` | 64 × 64 | Simplified one-color mark at 24 px and below. |
| `sigil-mark-micro-dark-mode.svg` | 64 × 64 | Simplified mark on dark backgrounds. |
| `sigil-wordmark.svg` | 452 × 226 | W1 wordmark on light backgrounds. |
| `sigil-wordmark-dark-mode.svg` | 452 × 226 | W1 wordmark on dark backgrounds. |
| `sigil-lockup.svg` | 692 × 226 | Preferred horizontal logo lockup. |
| `sigil-lockup-dark-mode.svg` | 692 × 226 | Horizontal lockup on dark backgrounds. |
| `sigil-mark.png`, `sigil-mark-2x.png` | 512² / 1024² | Raster mark fallbacks. |
| `sigil-wordmark.png`, `sigil-wordmark-2x.png` | 452 × 226 / 904 × 452 | Raster wordmark fallbacks. |
| `sigil-lockup.png`, `sigil-lockup-2x.png` | 692 × 226 / 1384 × 452 | Raster lockup fallbacks. |

## Compatibility assets

Existing public URLs are retained so repository docs, Pages, release archives, and package listings do not break. The `staff-glow` name is historical; these files now wrap the same flat master geometry.

| File | Canvas | Use |
| --- | ---: | --- |
| `sigil-full-staff-glow.svg` | 1094 × 545 | Light-background README and site hero compatibility wrapper. |
| `sigil-full-staff-glow-dark-mode.svg` | 1094 × 545 | Dark-background site hero compatibility wrapper. |
| `sigil-full-staff-glow.png`, `sigil-full-staff-glow-2x.png` | 1094 × 545 / 2188 × 1090 | Open Graph and raster compatibility. |
| `sigil-mark-staff-glow.svg`, `sigil-mark-staff-glow-dark-mode.svg` | 445 × 495 | Standalone mark compatibility wrappers. |
| `sigil-mark-staff-glow.png`, `sigil-mark-staff-glow-2x.png` | 445 × 495 / 890 × 990 | Raster mark compatibility. |
| `sigil-mark-staff-glow-watermark.svg`, `sigil-mark-staff-glow-watermark-4x.png` | 445 × 495 / 1780 × 1980 | Homepage watermark. |
| `sigil-wordmark-header.svg`, `sigil-wordmark-header-2x.png` | 527 × 226 / 1054 × 452 | Header compatibility wrapper and fallback. |

`sigil-full-soft*`, `sigil-full-strong*`, and `preview-*` are legacy comparison assets. Keep them only for historical compatibility; do not use them on new surfaces.

## Usage rules

- Use the flat logo by default and add glow only around the cyan staff at large display sizes.
- Keep the three color roles intact; do not recolor the terminal prompt to match the staff.
- Use the micro mark at 24 px and below instead of shrinking the detailed prompt and branches.
- Do not stretch the mark or wordmark; preserve each asset's viewBox aspect ratio.
