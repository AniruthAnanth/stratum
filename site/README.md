# Stratum landing page

Source for <https://aniruthananth.github.io/stratum/>. One statement, four
chips, one docked button. Next.js App Router, static export, `basePath`
`/stratum`, served by GitHub Pages from the `gh-pages` branch.

```
app/layout.tsx         metadata, next/font (Bricolage Grotesque), pre-paint html.js script, noscript CTA fix
app/page.tsx           <main>: sr-only h1, <Statement/>, <CtaDock/>
app/globals.css        palette, type, chip boxes, CSS typing, pre-paint dim state, reduced-motion
app/icon.svg|png       favicons (Next emits them); app/apple-icon.png
components/Statement.tsx   the 37 words + chips; scroll clock, pops, unfurls, "2005."
components/CtaDock.tsx     "Star on GitHub", slides in on the expo-out curve
public/og.png          1200x630 link preview (mark + wordmark + the price line)
scripts/postbuild.mjs  writes out/.nojekyll, asserts every asset is under /stratum/
scripts/serve.mjs      serves out/ at http://localhost:8123/stratum/ for a local look
```

## Build

```
pnpm install
pnpm build      # next build (Turbopack, static export) -> out/
pnpm serve      # http://localhost:8123/stratum/
```

Node 22, pnpm 9.15.0. `next/font` fetches the font from Google Fonts at build
time and self-hosts the woff2 — the live page makes no external requests.

## Deploy

`.github/workflows/site.yml` builds on every push to `main` that touches
`site/**` and publishes `out/` to `gh-pages` (peaceiris/actions-gh-pages,
`force_orphan`). Pull requests get the build only. By hand: push the contents
of `out/` (including `.nojekyll`) to the root of `gh-pages`.

## Motion, in one paragraph

Words ink in from 40% to full on a single `useScroll` clock over the paragraph
(`offset: ["start 0.55", "end 0.62"]`), color only. Round chips (mark, do-file)
pop with `cubic-bezier(0.68, -0.55, 0.27, 1.55)`; rectangle chips (terminal,
regression card) unfurl from width 0 on `cubic-bezier(0.19, 1, 0.22, 1)` and
only then play their contents. "2005." gets the one big pop with the teal
underline. The CTA slides up over page progress [0.7, 0.95] on the expo curve.
Hover pills are gated to `(hover: hover)`; keyboard gets them via
`:focus-visible`. No JS / reduced motion: full ink, whole chips, pill docked —
`html.js` is only added by the pre-paint script when motion is allowed.
Statement type is `clamp(2.3rem, 7vw, 5.2rem)` (7vw, not the brief's 6vw, to
leave the reveal enough scroll runway).
