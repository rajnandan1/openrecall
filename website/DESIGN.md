---
version: "alpha"
name: "Fambly"
description: "Warm-white paper, flat sticker mascots, beige 12px panels, black pill buttons, drawn phones and colour-coded tick lists."
colors:
  primary: "#171717"
  on-primary: "#ffffff"
  surface: "#ffffff"
  surface-container: "#fbfaf9"
  on-surface: "#121212"
  on-surface-variant: "#848281"
  outline: "#efedea"
  heading: "#343433"
  body: "#474645"
  pill: "#f6f4ef"
  pill-hover: "#eae6dd"
  stone: "#f2ebe0"
  blue: "#3784f4"
  app-blue: "#018dff"
  sky: "#62bfff"
  green: "#34c759"
  orange: "#ff5310"
  gold: "#ca9230"
  yellow: "#ffbe4c"
  pink: "#f966ac"
  purple: "#9553f9"
  red: "#ff3d17"
typography:
  headline-display:
    fontFamily: "Plus Jakarta Sans"
    fontSize: 68px
    fontWeight: 500
    lineHeight: 1.1
    letterSpacing: -0.045em
  headline-lg:
    fontFamily: "Plus Jakarta Sans"
    fontSize: 44px
    fontWeight: 500
    lineHeight: 48px
    letterSpacing: -0.045em
  body-md:
    fontFamily: "Inter"
    fontSize: 17px
    fontWeight: 400
    lineHeight: 26px
    letterSpacing: -0.01em
  label-md:
    fontFamily: "Inter"
    fontSize: 15px
    fontWeight: 600
    letterSpacing: -0.01em
rounded:
  sm: 6px
  md: 12px
  lg: 12px
  full: 32px
spacing:
  unit: 4px
  section: 96px
  max-width: 1024px
---

# Fambly

> A friendly phone money app on warm-white paper: flat sticker mascots flank a centred headline, everything else lives in beige 12px panels next to drawn phones.

Source: https://flavors.design/f/fambly

## Overview

The consumer-crypto wallet that decided money should feel like a sticker book.
White page, a 1024px column, and a hero whose headline sits between two big
clusters of soft clay-rendered mascots on pure white (a blue flower-blob with
a square face, a sleepy green bean, a red cloud, gold coins, a heart, a pink
piggy bank), shipped as real images, not vector art. Below that
the page turns calm and product-page quiet: beige `#fbfaf9` panels at 12px radius
holding drawn phones and UI fragments, 44px tight headlines, and small
colour-coded eyebrows and tick lists that give each section its own hue
(green, orange, blue, gold). The emotion is "relieved": finance without
dread. It is not a dark crypto dashboard, not glassmorphism, not a gradient
startup page; nothing glows, nothing is neon.

## Colors

| Role       | Value     | Notes |
| ---------- | --------- | ----- |
| bg         | `#ffffff` | page |
| bg-2       | `#fbfaf9` | beige panels, stats band, final CTA band |
| fg         | `#121212` | section headlines, card titles |
| heading    | `#343433` | hero h1, nav labels, wordmark |
| body       | `#474645` | paragraphs |
| fg-muted   | `#848281` | captions, footer links, "Watch the demo" |
| accent     | `#171717` | primary pill button |
| accent-fg  | `#ffffff` | text on accent |
| border     | `#efedea` | 1px section rules, inset card rings |
| pill       | `#f6f4ef` | secondary pill ("Log In", "Watch the Video") |
| pill-hover | `#eae6dd` | secondary pill hover |
| stone      | `#f2ebe0` | sticker backdrops, soft discs |
| blue       | `#3784f4` | Secure / details eyebrows |
| app-blue   | `#018dff` | app-UI blue, "Download the app" link |
| sky        | `#62bfff` | mascot body, shields |
| green      | `#34c759` | Simple eyebrow + ticks |
| orange     | `#ff5310` | Readable eyebrow, FAQ +, "See More FAQs" |
| gold       | `#ca9230` | coin shadow, gold eyebrow |
| yellow     | `#ffbe4c` | coins, stars |
| pink       | `#f966ac` | app icon tint |
| purple     | `#9553f9` | app icon tint |
| red        | `#ff3d17` | hearts, red mascot |

Scheme: light. Contrast rule: body `#474645` on white ≥ 9:1; muted `#848281` only for 13–15px captions.
Color rules: the page is achromatic; saturated colour appears only in stickers, app icons and one hue per section (eyebrow + its tick list share it). Primary buttons are always near-black, never coloured. No gradients except the tiny demo-video thumbnail.

## Typography

- Display: `"Plus Jakarta Sans", -apple-system, sans-serif` — weight 500 only, tracking `-0.045em` (the reference's custom grotesk sits at -0.02em; Jakarta is wider so it needs more), sentence case with a full stop.
- Body: `"Inter", -apple-system, sans-serif` — 17px/26px weight 500 for the hero sub, 19px/27px for section ledes, 15px/22px at 70% for card copy.
- Labels: Inter 15px weight 600, `-0.01em`, coloured, sentence case ("Simple", "Readable") — never uppercase.
- Scale: 11 / 13 / 14 / 15 / 17 / 19 / 24 / 44 / 68
- Load: `<link href="https://fonts.googleapis.com/css2?family=Plus+Jakarta+Sans:wght@500;600&family=Inter:wght@400;500;600&display=swap">`
- Rules: headlines are 2 lines max, end with a period, never bold. Tick-list items are 17px/500 in the section hue. Buttons are Inter 500, tracking `-0.03em`.

Plus Jakarta Sans stands in for the reference's proprietary house face: both are geometric grotesks with a single-storey-free double `a`, round `o`, short descenders and flat terminals.

## Layout

- Max width 1024px (+24px gutters); hero stickers bleed to the viewport edges.
- Base unit 4px; spacing 4 / 8 / 12 / 16 / 24 / 32 / 64 / 128.
- Section rhythm ~88–112px, with a 1px `--border` rule between most feature sections inside the column.
- Grid: hero centred 500px column between two 460px sticker clusters; bento 3 columns, 34px gap, first card spans 2 rows; feature sections are 50/50 splits (text / 532px-tall panel) alternating sides; details section is a sticky left title with a right column of stacked panels; FAQ is title-left / accordion-right.
- Density: airy — lots of white, content blocks small and quiet.
- Full-bleed `--bg-2` bands only for the stats and the final "Explore" CTA.

## Elevation & Depth

Flat paper with UI fragments that float a few millimetres; depth comes from the beige panel against white and soft rings, not shadows.

- Panels: `background: #fbfaf9`, no border, no shadow.
- Testimonial cards: white with `box-shadow: inset 0 0 0 1px var(--border)`; on hover they swap to `--bg-2` and the ring drops to 0 (100ms).
- UI fragments inside panels (transaction rows, the "Weekly" speed chip): white, ring `inset 0 0 0 1px var(--border)` plus `0 2px 10px color-mix(in srgb, var(--fg) 4%, transparent)`; the details transaction card fakes a stacked card behind it with `0 10px 0 -4px var(--bg), 0 11px 0 -3px var(--border)`.
- Phones: `10px solid var(--fg)` bezel, 56px radius, a 2px lighter outer ring, cropped by the panel's bottom edge.
- The only real drop shadow: highlighted pricing tier `0 3px 16px color-mix(in srgb, var(--fg) 10%, transparent)` (the site uses it on nav dropdowns).
- Forbidden: glass, blur, glows, noise, gradient borders.

## Shapes

- Radius: 12px panels and cards (`--radius`); 10px testimonial cards; 32px pills (buttons); 6px chips; 56px phones; 26px the dark action stack; 4px demo thumbnails.
- Circles for app icons (36px), avatars (30–40px) and sticker bubbles (72/90px).
- Stickers are flat vector shapes: circles-cluster blobs, rounded squares with two oval eyes, coins with two diagonal glare bars, 5-point stars, 4-point sparkles.

## Components

- Buttons: 32px-tall pills, padding 0 14px, Inter 500 15px — primary `#171717` → `#121212` on hover; secondary `#f6f4ef` → `#eae6dd`. Hero size 48px tall, padding 0 24px 0 20px, 17px, with a 20px glyph (download arrow / play triangle).
- Nav: 94px tall, white, sticky. Wordmark (18px mark + 17px bold name), then 15px/500 labels with 10px chevrons, 24px apart; "Log In" (secondary pill) and "Get Started" (primary pill) at the right. Mobile: wordmark, Get Started, two-line burger.
- Bento cards: `--bg-2`, 12px radius, content art on top, title 17px/600 fg, body 15px 70% body colour, 23px side padding.
- Dark action stack: near-black 26px-radius panel bleeding off the card's right edge, rows at 22px radius with a 36px coloured circle icon, 18px/500 title, 14px 62% white body, and an outlined 11px tag.
- Split feature: coloured eyebrow → 44px headline → 19px lede → coloured tick list (17px/500, 17×13 check stroke) → a "Watch the demo" row (78×44 thumbnail + two-line label) whose hover draws a 1px inset ring.
- Testimonials: two marquee lanes of 475px white ringed cards with avatar, bold name, muted handle, speech-bubble glyph top-right; edges masked to transparent.
- FAQ: rows separated by 1px rules, an orange "+" at 26px/300, question 19px fg; then an orange "See More FAQs →" link.
- Final CTA band: `--bg-2`, 44px headline, lede, blue "Download the app →" link, garden illustration right.
- Footer: mark, three 13px link columns (heading fg/500, links muted), copyright right.

## Do's and Don'ts

- Do flank the hero headline with two clusters of clay-rendered mascots and coins: generated raster images on a pure white backdrop (`mix-blend-mode: multiply`), never hand-drawn SVG scenes. Keep SVG for icons and small stickers.
- Do keep every primary button a near-black pill and every secondary a beige pill.
- Do give each feature section one hue and use it for the eyebrow and the tick list only.
- Do put product UI (drawn phones, transaction rows) inside beige 12px panels and crop the phone at the panel edge.
- Do end headlines with a period and keep them at weight 500.
- Do separate sections inside the column with 1px `--border` rules.

- Don't colour a primary button or add gradients to buttons.
- Don't uppercase labels or track them out.
- Don't give panels borders or drop shadows.
- Don't use dark mode crypto tropes: neon, glow, charts, candlesticks.
- Don't draw stickers with outlines or gradients; they are flat fills with at most a glare stripe.
- Don't use more than one saturated hue per section outside the stickers.

## Motion

- Duration 100ms for pill backgrounds and card hovers, 200ms ease-out for link colour; easing `cubic-bezier(.19,1,.22,1)` for anything that travels.
- What animates: button background, testimonial card fill/ring, demo-row ring, link colour, the two testimonial marquee lanes (60s / 72s linear, opposite directions, paused on hover), the spinner in "Checking Chore Photo".
- What never animates: headlines, stickers on load, panels on scroll.
- Signature move: the dual testimonial marquee with masked edges.

## Tweaks

- Signature knobs: mascot size (`--art-scale`), confetti opacity (`--sticker-opacity`), panel corners (`--radius-lg`), button corners (`--radius-pill`), headline tracking (`--title-track`), phone bezel (`--phone-bezel`).
- Alternate schemes: **Bedtime** (dark), **Mint**, **Butter**, **Grape**.

## Reference CSS

```css
:root {

--bg:#ffffff;
--bg-2:#fbfaf9;
--fg:#121212;
--heading:#343433;
--body:#474645;
--fg-muted:#848281;
--accent:#171717;
--accent-fg:#ffffff;
--border:#efedea;
--pill:#f6f4ef;
--pill-hover:#eae6dd;
--stone:#f2ebe0;
--blue:#3784f4;
--app-blue:#018dff;
--sky:#62bfff;
--green:#34c759;
--orange:#ff5310;
--gold:#ca9230;
--yellow:#ffbe4c;
--pink:#f966ac;
--purple:#9553f9;
--red:#ff3d17;
--font-display:"Plus Jakarta Sans", -apple-system, BlinkMacSystemFont, sans-serif;
--font-body:"Inter", -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
--radius:12px;
--radius-sm:6px;
--radius-lg:12px;
--radius-card:10px;
--radius-pill:32px;
--space-1:4px;
--space-2:8px;
--space-3:12px;
--space-4:16px;
--space-5:24px;
--space-6:32px;
--space-7:64px;
--space-8:128px;
--shadow:0 3px 16px color-mix(in srgb,var(--fg) 10%,transparent);
--ring:inset 0 0 0 var(--ring-w) var(--border);
--ease:cubic-bezier(.19,1,.22,1);
--dur:100ms;
--max-width:1024px;
--art-scale:1;
--sticker-opacity:1;
--title-track:-0.045em;
--ring-w:1px;
--phone-bezel:10px;
}
.btn{display:inline-flex;align-items:center;justify-content:center;gap:8px;height:32px;padding:0 14px;border:0;border-radius:var(--radius-pill);background:var(--accent);color:var(--accent-fg);font:500 15px/1 var(--font-body);letter-spacing:-.03em;transition:background-color var(--dur)}
.btn:hover{background:var(--fg)}
.btn.soft{background:var(--pill);color:var(--fg)}
.btn.soft:hover{background:var(--pill-hover)}
.card{padding:0 23px 16px;border-radius:var(--radius-lg);background:var(--bg-2);overflow:hidden}
.quote{padding:32px;border-radius:var(--radius-card);background:var(--bg);box-shadow:var(--ring);transition:box-shadow var(--dur),background var(--dur)}
.quote:hover{background:var(--bg-2);box-shadow:inset 0 0 0 0 var(--bg-2)}
.ticks li{gap:16px;font-size:17px;font-weight:500;color:var(--c)}
```

## Reference markup

```html
<section class="split">
  <div>
    <p class="eb c-green">Simple</p>
    <h2 class="t-display h2">Watch every jar grow.</h2>
    <p class="lede">Keep an eye on unlimited savings jars across every kid.</p>
    <ul class="ticks c-green">
      <li class="row"><svg><use href="#tick" /></svg>Watch Any Jar</li>
      <li class="row"><svg><use href="#tick" /></svg>Goal Pictures</li>
    </ul>
    <a class="demo" href="#"><i class="thumb"></i><span><b>Savings Jars</b><span>Watch the demo</span></span></a>
  </div>
  <div class="shot">
    <div class="phone">
      <div class="sbar row">9:38<i></i></div>
      <div class="tok row"><i class="av" style="--c:var(--green)">B</i><div>Bike fund<small>Goal $260</small></div><em>$214.50</em></div>
    </div>
  </div>
</section>
```

## How to apply this flavor

1. Replace the target page's design tokens with the palette, fonts and spacing above.
2. Rebuild the hero as a centred headline between two sticker clusters; restyle every other section as white column + beige 12px panels with drawn product UI.
3. Give each feature section one hue (eyebrow + tick list), make buttons black/beige pills, and apply the motion rules; remove any animation not described here.
4. Check the Don't list before finishing.

