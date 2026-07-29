# apps/admin (serpotter-admin)

**Generated:** 2026-07-29 · Vite+ SPA (not a Cargo member)

## OVERVIEW

React 19 admin UI: TanStack Router/Query, Base UI Cobalt, Vite+ (`vp`). Served at the **site root** (`/`) via `ADMIN_SPA_DIR`. Dual auth: admin session/secret vs playground `tok-`.

## STRUCTURE

```
src/
├── main.tsx / router.tsx / routeTree.gen.ts
├── routes/           # thin createFileRoute only
│   ├── login.tsx
│   └── _auth/        # pathless shell + panels
├── features/
│   ├── auth/         # snapshot, context, LoginPage, session-end
│   ├── shell/        # Shell, Topbar (page head), Sidebar (rail), CmdK, panel-status
│   ├── playground/   # tok- product calls
│   └── {stats,settings,tokens,keys,nodes,logs}/
├── lib/              # api, qk, query-client, safe-redirect, constants
└── components/ui/    # Base UI wrappers
```

## WHERE TO LOOK

| Task                | Location                                                                                         |
| ------------------- | ------------------------------------------------------------------------------------------------ |
| Route guard / login | `routes/_auth.tsx`, `routes/login.tsx` — **`getAuthSnapshot()`** in `beforeLoad`                 |
| Admin HTTP          | `lib/api.ts` `adminFetch` (SESSION then SECRET Bearer)                                           |
| Query keys          | `lib/query-keys.ts` hierarchical `qk.*`                                                          |
| 401 path            | `lib/session-end-app.ts` `endAdminSession`                                                       |
| Explicit logout     | `features/shell/Sidebar.tsx` rail foot — `auth.logout` + clear + nav (**not** `endAdminSession`) |
| Playground tok-     | `features/playground/` + `PLAY_TOKEN_KEY`                                                        |
| CmdK navigate       | `features/shell/Cmdk.tsx` Item **onClick** → `navigate`                                          |
| Page h1 + status    | `features/shell/Topbar.tsx` (title from `SECTIONS`) + `panel-status.tsx` context                 |
| Design system       | `/design.md` (locked) → `apps/admin/tokens.css` → `src/styles.css`                               |
| Build / base        | `vite.config.ts` — no `base` (root-served); `package.json` scripts                               |
| SPA fallback        | `crates/serpotter-api/src/lib.rs` `app_with_spa` — ServeDir + index.html fallback                |

## CONVENTIONS

- **Build SoT:** `npm run build` = `tsc -b && vp build` (local = CI = Docker). Node `^22.18.0 || >=24.11.0`.
- Root-served: no Vite `base`, no router `basepath`. Route `to` paths are already unprefixed (`/stats`).
- Thin routes → `features/*Panel`; no business logic in route files.
- Storage: `SESSION_KEY` preferred over `SECRET_KEY`; `PLAY_TOKEN_KEY` independent.
- Guards use module **auth snapshot** (same-turn lockstep with storage), not React context alone.
- Topbar Refresh = active panel `qk` only (playground disabled).
- Rail Console shape (see `/design.md`): rail → page head (**only** `h1`) → `.block` regions with `h2`. Panels render blocks, never cards, and publish status via `usePublishPanelStatus`.
- Data tables + metrics use `.bleed` to break `--view-pad`; first/last cell re-applies it.
- Credit sync honesty strings exact (partial throw / `errors=0` success) in keys queries.
- Dev proxy: `/api` `/live` `/ready` → `:8080`. Plugin order: `tanstackRouter()` **before** `react()`.

## ANTI-PATTERNS

- Never clear `PLAY_TOKEN_KEY` on logout/401 (`clearAuthStorage`).
- Never call `endAdminSession` from the explicit rail logout (double-clear).
- Never nest a bordered card around a bordered table, and never add a second `h1` to an authed page.
- Never use `--color-accent` for text on graphite — use `--color-accent-lift` (contrast).
- Never absolute / open redirects — `safeRedirectPath` path-only allowlist.
- Never hand-edit or oxfmt-thrash `routeTree.gen.ts`.
- Never dual plain-vite path — only `vp` / `npm run build`.
- Never set a Vite `base` or router `basepath` — a sub-path base breaks the server's index.html deep-link fallback.
- No Tailwind / Sonner / Radix / app-level Zod / optimistic toggles unless product asks.
- Zero `src/**/*.{js,jsx}` (`allowJs: false`).
