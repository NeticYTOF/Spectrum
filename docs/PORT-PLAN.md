# ascii-garden → Spectrum garden port plan

Direct port of the rendering algorithms from ascii-garden into the
terminal lyrics garden (mode 6). This is a code-level port, not an
independent interpretation: geometry, coverage classification and
palette come from the upstream source, adapted only where the terminal
replaces the HTML canvas.

## Upstream

- Repo: https://github.com/ridit-jangra/ascii-garden
- Revision ported: `f519e85f12a8e47b6c7b73a0d13a85043dedbc40`
- Files read: `src/engine/ascii.ts` (505 lines), `src/engine/garden.ts`
  (1642 lines), `README.md`, `LICENSE`, `docs/demo.gif`.
- License: MIT, Copyright (c) 2026 Ridit - full notice in `NOTICE`.
- Upstream credits preserved: Type Garden by Akshat Agarwal (the
  original design), ascii-animals by drewdi / DesignerStephen1 (the
  cell-grid ASCII approach). Their licences and copyrights remain
  their own.

## What is copied, what is not

- **Copied (as Rust re-implementations with full notice):** the role
  and coverage classifier, glyph families, line-glyph angle mapping,
  subpixel mask rasterization approach, letter layout/wrap algorithm,
  stem/leaf/rose/petal geometry and its constants, growth/word
  parameters, PRNG, sway/gust math, Paper palette row.
- **Not copied:** Playfair Display / Georgia font data (see Fonts),
  GSAP ticker, React UI, demo.gif, Type Garden assets.

## Fonts — the honest constraint

The reference renders each letter with `ctx.fillText` in
`700 Playfair Display, Georgia, serif` and measures advances with
`measureText`. A terminal binary cannot rasterize a system serif
without bundling a font engine and font data, and we do not ship
Playfair Display. Instead this port uses **original bitmap serif
letterforms** (thick stems, thin joins, bracket serifs, lowercase
x-height/ascender/descender proportions close to Playfair) defined in
code, with a per-glyph proportional advance table. They are drawn for
this project and are not extracted from any font file. Expect the
silhouettes to be recognizably serif but not identical to the
reference's type; that constraint is documented here rather than
hidden.

## Canvas → terminal geometry

| Reference (ascii.ts) | Terminal port |
|---|---|
| CSS pixels, `cw x ch` px cells, dpr | terminal cells; design space 1 cell = 1.0 x 2.0 units (aspect 1:2) |
| `SX=4, SY=6` samples/cell (fewer for small cells) | `SX=4, SY=8` subpixels/cell → square subpixels of 0.25 units; thresholds are fractional so they port unchanged |
| hidden mask canvas + `getImageData` + 15-bit nearest-role LUT | `MaskGrid { w, h, role: Vec<u8> }`; each sample written as a role index at paint time; nearest-role mapping used only for gradient shading |
| `present(t)`: per-cell moments, coverage, axis, glyph choice | same classifier verbatim (constants below), emitting terminal cells: solid → bg block fill + glyph, soft → fg glyph only, none → paper |

## Function map (ascii.ts → `src/garden/…`)

| Upstream | Port | Notes |
|---|---|---|
| `hash3`, `clamp`, `mix`, `tones` (dark = mix black 0.46, light = mix white 0.42), `onFill`, `lineGlyph` | `mask.rs` | identical math |
| roles 0-8: bg, flower, stem, flower-line, type, flower shadow/highlight, leaf shadow/highlight | `mask.rs` `Role` | 9 roles, Paper palette: `#F3EEE4, #FF3B1F, #1C2B8F, #FFD2C4, #111111` |
| `BASE_SETS`, `BASE_FAM`, `BASE_TONE`, `RAMP` | `mask.rs` | same glyph strings |
| coverage: `minHits = max(1, round(N*0.125))`, `secHits = max(1, round(N*0.17))`; solid if `cov >= 0.8` or (`cov >= 0.58` and ratio < 2.3); soft if `cov >= 0.1` | `mask.rs` | N = 32 here |
| `axis()`: covariance principal axis, `theta = 0.5*atan2(2cxy, cxx-cyy)`, `ratio = (mean+rad)/(mean-rad+0.35)` | `mask.rs` | identical |
| soft glyph: `lineish = 1.5` (type) / `1.9` (else); `lineGlyph(theta)` if ratio above, else `RAMP[clamp((cov-0.1)/0.5*8)]` | `mask.rs` | identical |
| solid cell: fill mixed to bg by tone (0/0.3/0.2), `onFill` glyph color, glyph chosen by `hash3(key…) % set.len`, secondary family ≥ secHits → its color + `lineGlyph`/`o`, 4.5% sparkle | `mask.rs` | identical |
| black text stays solid, no bg mixing | `mask.rs` | identical |

## Function map (garden.ts → `src/garden/…`)

| Upstream | Port | Notes |
|---|---|---|
| `mw(ch)` Playfair advance | `font.rs` advance table (em units) | proportional widths |
| `layoutCore` (wrap words, split over-long words, binary-search size, `LH=2.1`, `S0=H*0.28`, `maxW=W*0.8`, col `W*0.58`, height cap `H*0.82`) | `layout.rs` | verbatim; **all words must render** — no dropping |
| `wordParams` (`roseP, leafy, dens, lean, big, curvy, bridgeP, len`), chaos 0.3 | `botany.rs` | verbatim |
| `rng(seed)` (sfc32-style with `0x6D2B79F5`) | `botany.rs` | verbatim |
| `bez`, `at`, `vine`, `curl`, `weave`, `layerAt`, `strokeRange` | `botany.rs` | identical math |
| `grow`/`gen`/`genEnd` (stem anchors at letter top/bottom, bridges between letters, tips rose/fan/leaf/curl, thorns, depth-1 branches) | `botany.rs` | typing-only parts (wither, recoil) skipped; per-line regrow on line change |
| `leaf` (two-tone halves + light midrib) | `botany.rs` | identical |
| `rose` (6 petals back-to-front, spiral + sepal lines, radial stops redL/red/redD, `spr` spring) | `botany.rs` | identical |
| `jit`/`wpt` gust sway (`f = now/420`, gust sines) | `botany.rs` | animation time = track position → `--freeze-at` stays fully deterministic |
| `paint`/`render` layered passes | single mask with painter's z-order | terminal has one cell grid; z-order is preserved at write time (back flowers → text → front flowers), documented as the adaptation |

Not ported: other species (`drawKind` tulip/daisy/poppy/…), ocean/crystal
themes, butterflies/flies, poster/video export, touch/IME input. The
botanical rose + stems + leaves is the reference's signature look;
species variety can be a follow-up.

## Rendering pipeline (new)

`ui::render_lyrics` → `garden::render_garden` →
`layout` (wrap + size) → `gen` elements per letter (seeded PRNG) →
paint per frame into `MaskGrid` (roles) → `present` → terminal cells.
Colors: truecolor ANSI (fg glyph + bg block fill); 256/16-color modes
quantize the same palette.

## Preserved invariants

- Real-identity gate for lyrics (unchanged).
- Full text always renders: wrap or split, never truncate mid-lyric
  (fallback to plain text only for unsupported scripts).
- Deterministic frozen timeline: all animation phases derive from the
  track position, so `--freeze-at` frames are byte-identical.
- Mixed-case lyric text rendered as-is (no uppercasing).

## Testing plan

- classifier: hand-built masks → expected cell class/glyph family.
- layout: all words present at 144x42/100x30/72x22/tiny; long words
  split; never clipped mid-word; fallback path for non-Latin.
- geometry: rose/leaf/stem outputs stay inside the mask; growth
  timings monotonic; PRNG reproducibility.
- determinism: two renders at the same frozen position are identical
  across wall-clock time; live render advances with position.
