---
version: "alpha"
name: "OpenRecall"
description: "Lowercase mono on white graph paper: hairline figure frames, dotted grids, square buttons, the OpenRecall logo orange as ink and one apricot CTA."
colors:
  primary: "#c2410c"
  on-primary: "#ffffff"
  surface: "#ffffff"
  surface-container: "#fbf8f4"
  on-surface: "#1c1917"
  on-surface-variant: "#57534e"
  outline: "#d9d8e2"
  fg-dim: "#a8a29e"
  accent-2: "#ea580c"
  cta: "#fdba74"
  cta-hover: "#fb923c"
  key: "#fff7ed"
  night: "#1c1917"
  cta-fg: "#1c1917"
  cover-bg: "#1c1917"
  ok: "#3f6b3f"
  err: "#b23a2f"
typography:
  headline-display:
    fontFamily: "Azeret Mono"
    fontSize: 56px
    fontWeight: 500
    lineHeight: 1.05
    letterSpacing: -2px
  headline-lg:
    fontFamily: "Azeret Mono"
    fontSize: 34px
    fontWeight: 500
    lineHeight: 1.3
    letterSpacing: -2px
  body-md:
    fontFamily: "Azeret Mono"
    fontSize: 13px
    fontWeight: 500
    lineHeight: 1.65
  label-md:
    fontFamily: "Azeret Mono"
    fontSize: 12px
    fontWeight: 600
  label-sm:
    fontFamily: "Azeret Mono"
    fontSize: 12px
    fontWeight: 400
    letterSpacing: 0.14em
rounded:
  none: 0px
  md: 0px
  pill: 20px
spacing:
  unit: 4px
  section: 64px
  max-width: 1180px
components:
  button-primary:
    backgroundColor: "{colors.on-surface}"
    textColor: "{colors.surface}"
    rounded: "{rounded.none}"
    padding: 9px 14px
  button-accent:
    backgroundColor: "{colors.cta}"
    textColor: "{colors.cta-fg}"
    rounded: "{rounded.none}"
    padding: 9px 14px
  subnav:
    backgroundColor: "{colors.primary}"
    textColor: "{colors.on-primary}"
    height: 64px
  figure:
    backgroundColor: "{colors.surface-container}"
    textColor: "{colors.on-surface}"
    rounded: "{rounded.none}"
  fig-bar-rule:
    backgroundColor: "{colors.fg-dim}"
    textColor: "{colors.on-surface}"
  button-accent-hover:
    backgroundColor: "{colors.cta-hover}"
    textColor: "{colors.cta-fg}"
  grid-cell-lit:
    backgroundColor: "{colors.accent-2}"
    size: 18px
  layer-bar:
    backgroundColor: "{colors.surface-container}"
    textColor: "{colors.night}"
  story-cover:
    backgroundColor: "{colors.cover-bg}"
    textColor: "{colors.on-primary}"
  status-pass:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.ok}"
  status-fail:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.err}"
---

# OpenRecall

> An engineering notebook for a developer platform: lowercase monospace on white graph paper, every product shot drawn as a numbered figure in a hairline frame, the burnt orange of the OpenRecall logo as the one ink colour and one apricot button.

Source: https://flavors.design/f/wrap, recoloured to the OpenRecall logo in `assets/logo/`: orange `#C2410C` and warm ink `#1C1917`.

Everything is set in Azeret Mono, a wide geometric mono from Google Fonts; Inter covers the few sans labels inside figures.

## Overview

OpenRecall reads like a lab notebook that ships software. The page is white, laid over a 22px dot grid and six faint vertical column rules that run the full height of the page, so every section sits on visible graph paper. Everything is monospace, mostly lowercase, weight 500, tightly tracked in the headlines (-2px). Product is never screenshotted: it is drawn as **numbered figures** — a warm off-white panel with a hairline frame and a title bar reading `>_ ——— [ fig. 2 · quickstart ] ——— ⌗` — holding cell grids, YAML, line charts, run lists and a layered stack diagram.

Colour is almost absent: warm ink `#1c1917`, a stone grey for body, and one burnt orange `#c2410c`, the colour of the logo mark, used for eyebrows, numbers, links and, once, a full-bleed band. The single loud thing is an apricot `#fdba74` "install" button. It is for developer platforms, infra, CI and agent tooling. It is not dark-mode terminal cosplay, not glassy, not rounded, and never uses gradients as decoration.

## Colors

| Role      | Value                                              | Notes                                                     |
| --------- | -------------------------------------------------- | --------------------------------------------------------- |
| bg        | `#ffffff`                                          | paper, under the dot grid                                 |
| bg-2      | `#fbf8f4`                                          | warm off-white figure panels                              |
| fg        | `#1c1917`                                          | warm ink, the logo wordmark: headlines, primary button fill |
| fg-muted  | `#57534e`                                          | body copy, ledes, fig bars                                |
| fg-dim    | `#a8a29e`                                          | decorative glyphs and rules only, never text              |
| accent    | `#c2410c`                                          | logo orange: eyebrows, numbers, links, subnav, one band   |
| accent-fg | `#ffffff`                                          | text on orange                                            |
| accent-2  | `#ea580c`                                          | chart lines, lit grid cells (fills only, never text)      |
| cta       | `#fdba74`                                          | the one apricot CTA; buttons on orange hover to it        |
| cta-hover | `#fb923c`                                          | apricot button hover                                      |
| key       | `#fff7ed`                                          | subnav key brackets and eyebrows on orange                |
| night     | `#1c1917`                                          | base of every hairline (16%) and grid line (7%)           |
| cta-fg    | `#1c1917`                                          | text on the apricot CTA (stays dark in every scheme)      |
| cover-bg  | `#1c1917`                                          | dark cover behind the white wordmark                      |
| border    | `color-mix(in srgb,var(--night) 16%,transparent)`  | every frame and rule                                      |
| ok / wait / err | `#3f6b3f` / `#8a7d1a` / `#b23a2f`          | pass, pending, fail in tables and badges; olive, not neon |

Scheme: light. Contrast rule: ink on white is ~17.5:1, body grey ~7.6:1, orange on white ~5.2:1, white on orange ~5.2:1, cream on orange ~4.9:1. Dim grey `#a8a29e` is ~2.5:1, so it never carries text.
Color rules: orange is ink, not paint — it colours text and 1px outlines, and fills only the subnav, the primary button hover and one full-bleed band. Apricot appears on at most two buttons per page. Status colours are muted olive/ochre/brick. No gradients except the flat tints used to draw grids and hover washes.

## Typography

- Display: `"Azeret Mono", ui-monospace, monospace` — weight 500, 56px hero / 34px section, line-height 1.05 / 1.3, tracking -2px, sentence case
- Body: same mono — 13px/1.65 weight 500, grey; ledes 13.5px, figure copy 12–12.5px
- Labels: 12px. Short labels (chips, badges, `LAYER 01`) are uppercase with 0.1–0.14em tracking; fig titles are lowercase with 0.04em; eyebrows are lowercase `# section name` in orange. Nothing is set under 12px.
- Sans (Inter): only inside figures — layer titles, chart labels
- Scale: 12 / 12.5 / 13 / 13.5 / 18 / 22 / 34 / 56
- Load: `<link href="https://fonts.googleapis.com/css2?family=Azeret+Mono:wght@400;500;600&family=Inter:wght@400;500&display=swap">`
- Rules: buttons and nav are lowercase at 600; feature titles lowercase with the first letter capitalised; never bold above 600; never italic

## Layout

- Max width 1180px with 24px gutters; the column rules sit at sixths of that width
- Base unit 4px; spacing 4 / 8 / 14 / 20 / 24 / 32 / 48 / 64
- Section rhythm: 64px top and bottom, each section closed by a full-bleed 1px rule
- Every section: orange `# eyebrow`, 34px headline, one-line grey lede, then a full-width figure 36px below
- Grids: hero is left-aligned text only (the figure below does the showing); features are a 50/50 rail (clickable list left, figure right); quality loop is three equal figure columns; the speed band is a 50/50 orange band; stories are three columns; footer is five columns
- Density: balanced — lots of paper around sections, dense tabular content inside figures

## Elevation & Depth

Flat graph paper. Depth comes from hairlines and the dot grid, not from shadows.

- Page: `radial-gradient(circle at 1px 1px, night-at-7% 1px, transparent 0) 0 0/22px 22px` over white, plus a centred `::before` with 1px `night-at-7%` borders and five 1px linear-gradient column lines at sixths.
- Frames: `1px solid color-mix(in srgb, #1c1917 16%, transparent)` on figures, buttons (ghost), fig-bar icons, stat cells, tier dividers, FAQ rows. Rows inside run lists use the softer 7% line.
- Figure panels are `#fbf8f4` with their own denser dot grid (`1.2px` dots every 26px).
- Nav is separated by a dotted rule: `radial-gradient(circle, border 1px, transparent 1.1px) 0 50%/7px 3px`, 3px tall.
- Hover and active wash: `linear-gradient(night-7%, night-7%), var(--bg)` — a flat tint, never a lift.
- The only shadow is `0 8px 22px color-mix(in srgb, #1c1917 10%, transparent)` on the floating benchmark tooltip. The picked pricing tier gets `inset 0 2px 0 var(--accent)`.

## Shapes

- Radius 0 on everything structural: buttons, figures, cards, inputs, stat cells, covers, layer bars
- 20px pills only for status chips (LIVE, run badges, layer tags); 50% circles for the live dot, run dots and avatars
- Grid cells are 18px squares; badges are square-cornered outlined rectangles
- The logo is the OpenRecall loop mark in orange `#C2410C` beside the wordmark in ink, from `assets/logo/`

## Components

- **Numbered figure (signature).** Frame + `fig-bar`: `>_` icon box, a 1px rule, `[ fig. N · name ]` in 12px body-grey mono at 0.04em, another rule, a `⌗` icon box. Body is the warm off-white dotted panel. Use it for every product visual.
- **Cell grid hero figure.** 16×7 grid of 18px outlined squares with a handful filled `accent-2` (count is a knob), one dash, an uppercase `KITCHEN.YAML` edge label and a `● LIVE` pill, then a four-cell stat row underneath.
- **Feature rail.** Left: stacked buttons, 20px padding, orange `01` number, 18px/600 title, 12.5px grey body; the active row gets the 7% wash. Right: a figure whose `<pre>` swaps to the matching YAML/CLI snippet (keys orange, strings olive, comments in body grey).
- **Quality loop.** Three figure columns: title, outlined `PASS`/`BEST` badge plus a 20px number, a thin `accent-2` polyline chart with dim axis labels, and a three-row table with pass/fail in olive/brick.
- **Orange band.** Full-bleed `#c2410c` with a white dot grid, 28px white headline at line-height 1.65, a white button with orange text that hovers to apricot, and a white chart card with an orange-edged tooltip.
- **Layer stack.** Four horizontal bars, each wider than the one above, filled with orange at 5% / 14% / 30% / 100%; tiny uppercase `LAYER 01`, a sans title, pill tags and right-aligned mono copy.
- **Control plane.** Stacked area chart in three tints next to a run list: dot, truncated task, model pill, coloured source pill, round initial avatar, age.
- Buttons: 600 12px mono, lowercase, `9px 14px`, square. Primary = ink fill → orange on hover; ghost = white with hairline → orange border and text; accent = apricot → `#fb923c`; nav CTAs add `letter-spacing:.02em` and more padding.
- Nav: 62px white bar, OpenRecall logo, five 12px/500 lowercase links with an orange underline on hover, a ghost "github ↗" and an apricot "install" button. After the hero a 64px orange **subnav** slides down: `[1] how it works  [2] the gate …` with the bracketed number in cream and a white "install openrecall" button.
- FAQ: full-width rows with a hairline under each, orange `>` prompt, lowercase question, `[+]` / `[−]` on the right.
- Footer: five columns, dim lowercase headings, ink 12px links that turn orange, then a hairline and "All Rights Reserved © 2026".

## Do's and Don'ts

- Do draw every product visual as a numbered figure with the `>_ — [ fig. N · name ] — ⌗` bar.
- Do keep the page on graph paper: 22px dot grid plus column rules at sixths, visible behind every section.
- Do set everything in one monospace at 500, lowercase for buttons, links and questions.
- Do use orange as ink (eyebrows, numbers, links, outlines) and fill with it only for the subnav and one band.
- Do reserve apricot for one or two call-to-action buttons.
- Do use real-looking data in figures: timestamps, costs, pass/fail, PR numbers.
- Do hover by recolouring (ink → orange, orange → apricot, border → orange), not by moving.

- Don't round corners on buttons, cards or figures; only chips are pills.
- Don't add drop shadows to cards or buttons.
- Don't use screenshots or photos inside the page chrome; draw the product.
- Don't use neon greens or reds for status; keep olive, ochre and brick.
- Don't set headlines in a sans or at positive tracking.
- Don't hardcode a colour outside `:root`; derive tints with `color-mix()`.
- Don't let gradients read as gradients; they only draw dots, rules and flat washes.

## Motion

- Duration 150ms, easing `ease`
- What animates: background, border-colour and colour on buttons, nav links, feature rows and footer links; the subnav slides in over 250ms once the hero is past
- What never animates: layout, figures, section entrances
- Signature move: the `● LIVE` dot blinks on a 2s `step-end`, and lit grid cells pulse from `accent-2` to `accent` in steps, like a status board
- All of it sits inside `@media (prefers-reduced-motion: no-preference)`

## Tweaks

Five knobs. `--dot-gap` (12–40px) is the page dot-grid pitch; `--grid-alpha` (3–16%) drives the column rules, grid dots and hover wash; `--head-track` (−4 to 1px) is the headline tracking; `--lit-cells` (0–30) is how many squares are lit in the hero figure, redrawn on `flavor:tweak`; `--stack-step` (0–12) is how much narrower each layer bar is than the next. Schemes: Night shift, Appliance, Terminal green, Magenta — each sets all sixteen colour literals.

## Reference CSS

```css
:root {
  --bg:#ffffff;
  --bg-2:#fbf8f4;
  --fg:#1c1917;
  --fg-muted:#57534e;
  --fg-dim:#a8a29e;
  --accent:#c2410c;
  --accent-fg:#ffffff;
  --accent-2:#ea580c;
  --cta:#fdba74;
  --cta-hover:#fb923c;
  --key:#fff7ed;
  --night:#1c1917;
  --cta-fg:#1c1917;
  --cover-bg:#1c1917;
  --ok:#3f6b3f;
  --wait:#8a7d1a;
  --err:#b23a2f;
  --border:color-mix(in srgb,var(--night) 16%,transparent);
  --line-soft:color-mix(in srgb,var(--night) var(--grid-alpha),transparent);
  --font-display:"Azeret Mono", ui-monospace, monospace;
  --font-body:"Azeret Mono", ui-monospace, monospace;
  --font-mono:"Azeret Mono", ui-monospace, monospace;
  --font-sans:"Inter", system-ui, sans-serif;
  --radius:0px;
  --radius-lg:0px;
  --radius-pill:20px;
  --space-1:4px;
  --space-2:8px;
  --space-3:14px;
  --space-4:20px;
  --space-5:24px;
  --space-6:32px;
  --space-7:48px;
  --space-8:64px;
  --shadow:0 8px 22px color-mix(in srgb,var(--night) 10%,transparent);
  --ease:ease;
  --dur:150ms;
  --max-width:1180px;
  --dot-gap:22px;
  --grid-alpha:7%;
  --head-track:-2px;
  --lit-cells:6;
  --stack-step:8;
}
.btn{display:inline-flex;align-items:center;gap:6px;padding:9px 14px;border:1px solid var(--border);border-radius:var(--radius);background:var(--bg);color:var(--fg);font:600 12px/1.65 var(--font-mono);text-transform:lowercase}
.btn-primary{background:var(--fg);border-color:var(--fg);color:var(--bg)}
.btn-primary:hover{background:var(--accent);border-color:var(--accent)}
.btn-accent{background:var(--cta);border-color:var(--cta);color:var(--cta-fg)}
.btn-accent:hover{background:var(--cta-hover);border-color:var(--cta-hover)}
.fig{border:1px solid var(--border);background:var(--bg-2)}
.fig-bar{display:flex;align-items:center;gap:14px;padding:8px 12px;border-bottom:1px solid var(--border);color:var(--fg-muted);font-size:12px;letter-spacing:.04em}
.fig-bar .rule{flex:1;height:1px;background:var(--border)}
.fig-bar .ic{padding:1px 5px;border:1px solid var(--border)}
```

## Reference markup

```html
<section class="sec"><div class="wrap">
  <div class="eyebrow"># taste loop</div>
  <h2>Baked-in measurement and self-seasoning</h2>
  <p class="lede">Taste tests, benchmarks, and self-improvement loops drive measurable crunch.</p>
  <figure class="fig">
    <div class="fig-bar">
      <span class="ic">&gt;_</span><span class="rule"></span>
      <span>[ fig. 4 · taste loop · sample shift ]</span>
      <span class="rule"></span><span class="ic">⌗</span>
    </div>
    <div class="three">
      <div>
        <h4>Taste tests on your own menu</h4>
        <div class="kpi"><span class="badge">PASS</span>96%</div>
        <table><tr><td>8/13 07:03</td><td class="fail">fail</td><td>0.4</td><td>$3.39</td></tr></table>
      </div>
    </div>
  </figure>
</div></section>
```

## How to apply this flavor

1. Replace the target page's design tokens with the palette, fonts and spacing above, and put the dot grid and column rules on the page background.
2. Restyle components to match the Components section; turn every screenshot into a numbered figure.
3. Apply the motion rules; remove any animation not described here.
4. Check the Don't list before finishing.
