# Cama bot guides (published site)

Player-facing guides for the Cama Balanced Shuffle bot, published as a static site
with GitHub Pages. This is documentation only — no Rust code, no build step, no
dependency on the runtime.

Live at **https://t4n1o.github.io/cama-mm/** once Pages is enabled
(Settings → Pages → Deploy from a branch → `/docs`).

## Why a site and not just links to a chat message

Guides get posted in the Discord `#bot-guides` channel. A bare link to a page that
sits behind a login or a bot challenge shows up in Discord as plain blue text: the
link unfurler fetches the URL, gets blocked or finds no metadata, and gives up.
Pages serves plain static HTML to anyone, so Discord reads the `og:` tags in each
page and renders a proper preview card with title, description and image.

## Layout

```
docs/
  index.html                 hub page linking every guide
  guides/<slug>/index.html   one self-contained guide per folder
  assets/fonts/              self-hosted woff2 + per-guide css (latin subsets only)
  assets/og/                 1200x630 preview cards + the card source HTML
  assets/discord/            pre-sliced PNGs for posting straight into Discord
  .nojekyll                  serve files as-is, no Jekyll build
```

Each guide page is standalone: its own `<style>`, no shared stylesheet, no JS.
Fonts are self-hosted so the page does not call Google Fonts at render time.

## Adding or editing a guide

1. Add or edit `guides/<slug>/index.html`.
2. Keep the `<head>` block from an existing guide and update `og:title`,
   `og:description`, `og:url`, `og:image` and `<title>`. Absolute URLs are
   required — Discord will not resolve a relative `og:image`.
3. Add a card for it in `index.html`.
4. Regenerate the preview card (below) and commit the PNG.

### Regenerating a preview card

Cards are rendered from `assets/og/<slug>-card.html` at exactly 1200x630. Any
headless browser works; with a Chromium available:

```bash
chromium --headless --disable-gpu --window-size=1200,630 \
  --screenshot=docs/assets/og/<slug>.png \
  docs/assets/og/<slug>-card.html
```

### Re-cutting the Discord images

`assets/discord/*.png` are section slices of the guide pages, sized so Discord
does not shrink them into thumbnails. Re-cut them by screenshotting the guide
page in dark mode at 880px wide, one section per image.

## Keeping it honest

These guides describe live bot behaviour. When a change lands that alters what a
guide claims — command names, timers, thresholds, who gets pinged — update the
guide in the same PR. A guide that disagrees with the code is worse than no guide.
