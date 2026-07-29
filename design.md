# Design — Serpotter Admin

Locked design system. Future Hallmark runs read this file first; pages defer
to it. Amend intentionally — the file is the rule.

## System
- Genre · modern-minimal
- Macrostructure · Rail Console (App Shell family, ops console)
- Theme · catalog Cobalt
- Axes · light / grotesk-sans / cool electric-blue

## Rail Console — the app-page shape

Every authenticated page is one shape. Pages differ in their blocks, never in
their chrome.

1. **Graphite rail** — the page's single dark beat (Cobalt signature 8, promoted
   from decoration to structure). Full-height and fixed at ≥64rem; a static
   two-row band below it. Carries wordmark, section nav, session chips, logout.
2. **Page head** — sticky, light, hairline-bottomed. One `h1` (the section name),
   a mono status readout fed by the active panel, and the actions (`Jump ⌘K`,
   `Refresh`). This is the only `h1` on an authenticated page.
3. **Blocks** — flush content regions separated by hairline rules, never nested
   cards. Each block owns an `h2` naming the operation ("Seed key", "Credit
   sync"), not a repeat of the page title.
4. **Full-bleed data** — the metrics strip and every table break the view's
   horizontal padding to the column edge; cell text stays aligned to it via
   `--view-pad` on the first/last cell.

### Bans specific to this shape
- No bordered card wrapping a bordered table (card-in-card).
- No `nth-child` border-stripping on the metrics strip — dividers come from
  `gap: 1px` over a rule-coloured grid background, so any count wraps correctly.
- No second `h1`, and no `h2` that restates the page title.
- No theme or system name in product copy.

## Tokens (canonical · `apps/admin/tokens.css` is the source of truth)
```css
:root {
  --color-paper:      oklch(98.5% 0.004 250);
  --color-paper-2:    oklch(96.5% 0.006 252);
  --color-ink:        oklch(24% 0.02 258);
  --color-ink-2:      oklch(34% 0.018 257);
  --color-muted:      oklch(45% 0.014 256);   /* floor for real text */
  --color-rule:       oklch(88% 0.008 255);   /* structural dividers */
  --color-rule-2:     oklch(58% 0.014 256);   /* control borders — 3:1 */
  --color-accent:     oklch(56% 0.2 256);     /* fill */
  --color-accent-strong: oklch(48% 0.19 256); /* accent as text on light */
  --color-accent-ink: oklch(98% 0.01 256);
  --color-focus:      oklch(56% 0.2 256);
  --color-graphite:   oklch(22% 0.016 260);

  /* Accent legible on the graphite rail (5.5:1) — never use --color-accent
     for text on graphite. */
  --color-accent-lift: oklch(70% 0.16 256);

  /* Status: base hue = border/fill (3:1); -text variant = readable copy (5.5:1+) */
  --color-error-text:   oklch(44% 0.17 25);
  --color-success-text: oklch(42% 0.12 155);
  --color-warn-text:    oklch(45% 0.11 75);

  /* Console chrome */
  --rail-w:   14.5rem;
  --head-h:   3.5rem;
  --view-pad: 1rem;   /* --space-xl at >=64rem; drives full-bleed alignment */

  --font-display: "Space Grotesk", ui-sans-serif, system-ui, sans-serif;
  --font-body:    "Inter", ui-sans-serif, system-ui, sans-serif;
  --font-mono:    "JetBrains Mono", ui-monospace, monospace;

  /* 4-pt spacing: --space-3xs … --space-3xl. See apps/admin/tokens.css. */
  /* Type scale ~1.25: --text-xs … --text-display. */

  --ease-out: cubic-bezier(0.16, 1, 0.3, 1);
  --dur-micro: 120ms;
  --dur-short: 220ms;
  --dur-long:  420ms;

  --radius-sm: 6px;
  --radius-md: 10px;

  --rule-w: 1px;          /* every border and hairline gap */
  --z-sticky: 200;        /* in-page sticky (page head) */
  --z-sticky-nav: 300;    /* the rail always out-paints */
}
```

## Contrast contract

Text colour never comes from a fill token. The pairs below are the whole rule:

| Use | Token | Floor |
| --- | --- | --- |
| Body / label / meta copy on paper | `--color-muted` or darker | 4.5:1 |
| Accent as text on a light surface | `--color-accent-strong` | 4.5:1 |
| Text on an accent fill | `--color-accent-ink` | 4.5:1 |
| Any text on graphite | `--color-graphite-ink` / `-muted` / `--color-accent-lift` | 4.5:1 |
| Status copy | `--color-{error,success,warn}-text` | 4.5:1 |
| Border that identifies a control | `--color-rule-2` / `--color-graphite-control` | 3:1 |
| Decorative divider | `--color-rule` | none |

## CTA voice
- Primary · accent fill · accent-ink text · 6px radius · 44px min height
- Secondary · paper fill · rule border · same radius · ghost for tertiary

## Motion stance
- Three primitives only · view-in (once per route change) · press · busy spinner
- No scroll-triggered reveals — a console is composed, not animated in
- Reduced-motion · ≤150 ms opacity crossfade; view-in becomes static

## Per-page allowances
- App pages MUST NOT use enrichment. Function carries the page.
- Blocks per page vary (1–3); chrome does not.
- The login gate is the one page allowed a two-column split: graphite aside +
  form column. It has no rail and no page head.

## Exports
`apps/admin/tokens.css` is the source of truth. For Tailwind v4 `@theme`,
DTCG `tokens.json`, or shadcn/ui CSS variables, ask *extend design.md with
Tailwind exports* (or the format you want).
