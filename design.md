# Design — Serpotter Admin

Locked design system. Future Hallmark runs read this file first; pages defer
to it. Amend intentionally — the file is the rule.

## System
- Genre · modern-minimal
- Macrostructure · App Shell (ops console)
- Theme · catalog Cobalt
- Axes · light / grotesk-sans / cool electric-blue

## Tokens (canonical · `apps/admin/tokens.css` is the source of truth)
```css
:root {
  --color-paper:      oklch(98.5% 0.004 250);
  --color-paper-2:    oklch(96.5% 0.006 252);
  --color-ink:        oklch(24% 0.02 258);
  --color-ink-2:      oklch(34% 0.018 257);
  --color-rule:       oklch(88% 0.008 255);
  --color-accent:     oklch(58% 0.2 256);
  --color-accent-ink: oklch(98% 0.01 256);
  --color-focus:      oklch(58% 0.2 256);
  --color-graphite:   oklch(22% 0.016 260);

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
}
```

## CTA voice
- Primary · accent fill · accent-ink text · 6px radius · 44px min height
- Secondary · paper fill · rule border · same radius · ghost for tertiary

## Motion stance
- Sparse · panel reveal · press · busy spinner only
- Reduced-motion · ≤150 ms opacity crossfade; reveals static

## Exports
`apps/admin/tokens.css` is the source of truth. For Tailwind v4 `@theme`,
DTCG `tokens.json`, or shadcn/ui CSS variables, ask *extend design.md with
Tailwind exports* (or the format you want).
