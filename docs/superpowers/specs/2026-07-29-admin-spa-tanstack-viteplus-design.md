# Admin SPA restructure — TanStack Router + Query + TypeScript + Vite+ + Base UI

**Date:** 2026-07-29  
**Status:** Approved (brainstorm)  
**Scope:** `apps/admin` only (plus CI admin job, Dockerfile `admin-build`, ops docs that name SPA build commands)  
**Backend:** frozen — no new admin REST/MCP endpoints

## Goal

Rebuild the Serpotter admin console as a strict TypeScript SPA on **Vite+ first**, with:

- **TanStack Router** — real URL per panel, `/login` + route guards
- **TanStack Query** — per-resource queries/mutations (kill mega-`refresh()`)
- **Base UI** + **Cobalt** — modern shell (sidebar, toasts, confirms, CmdK)
- **SPA-only UX** — filters, loading/empty states, AlertDialog; no product expansion

Cut over cleanly in one ship unit (image-baked `dist/`). Sessions in `localStorage` keep existing key names.

## Current state

| Aspect | Today |
|---|---|
| Stack | Vite 6 + React 19, plain JS |
| Nav | Single workbench; seven stacked panels; CmdK `scrollIntoView` |
| Data | `useAdminData` parallel `refresh()` + global busy/err/notice |
| Auth | `useAdminSession` + mount-swap `LoginGate` (no `/login` URL) |
| Build | `npm run build` → `vite build`; CI + Docker `node:22` + `npm ci` + `npm run build` |
| Base | Vite `base: '/admin/'`; served via `ADMIN_SPA_DIR` |

## Decisions (locked)

| Decision | Choice |
|---|---|
| Approach | **B — Vite+ first** (toolchain foundation before/with app rewrite) |
| Navigation | Real routes per panel |
| TypeScript | Strict full cutover (no remaining `.js`/`.jsx` under `src/`) |
| Query model | Per-resource queries + mutations |
| UI foundation | Cobalt CSS + **Base UI** (`@base-ui/react`) |
| Auth routing | Route guards + `/login` |
| Modernize depth | Chrome + SPA-only UX features (no new backend) |
| Zod | **No** — `adminFetch<T>` + interfaces |
| Optimistic toggles | **Out of v1** |
| Automated SPA tests | **Out of v1** (typecheck + build + manual smoke) |
| Use-in-playground | Set `playToken` only — **no** auto-navigate |
| Secret login | apply → navigate; bad secret → first query + global 401 |
| playToken on logout | **Survives** (`PLAY_TOKEN_KEY` untouched) |

---

## §1 Architecture & toolchain foundation

### Toolchain order (hard)

1. Vite+ foundation (Vite 8 + Rolldown via `@voidzero-dev/vite-plus-core`)
2. Strict TypeScript scaffold
3. TanStack Router (file-based) + auth layout
4. TanStack Query (per-resource)
5. Base UI chrome + panel modernize

### Runtime stack

| Layer | Choice |
|---|---|
| CLI / build | `vp dev` / `vp build` / `vp check` / `vp preview` (via npm scripts) |
| Config | `vite.config.ts` — `defineConfig` from **`vite-plus`** |
| Bundler | Vite 8 + Rolldown (Vite+ core override) |
| React | React 19 + **`@vitejs/plugin-react@^6`** |
| Router | `@tanstack/react-router` + `@tanstack/router-plugin` (file-based) |
| Data | `@tanstack/react-query` v5 — **no** `@tanstack/react-router-ssr-query` |
| UI | `@base-ui/react` ~1.6 (Dialog, AlertDialog, Menu, Toast, Autocomplete+Dialog) |
| Visual | Cobalt `tokens.css` + evolved CSS — **no Tailwind** |
| Types | Strict TS; plain DTO interfaces |

### Path / deploy contract

- Vite **`base: '/admin/'`**
- Router **`basepath: '/admin'`** (route `to` values unprefixed: `/login`, `/stats`, …)
- Dev URL: `http://localhost:5173/admin/`
- Proxies origin-root: `/api`, `/live`, `/ready` → `:8080`
- Production: multi-stage image → `/admin-dist`, `ADMIN_SPA_DIR=/admin-dist`
- Runtime only needs static files; no Node in production container

### App folder shape

```
apps/admin/src/
  main.tsx
  router.tsx
  routeTree.gen.ts          # plugin-generated; exclude from formatters
  routes/                   # thin file routes only
    __root.tsx
    login.tsx
    _auth.tsx               # pathless guard + Shell + <Outlet />
    _auth/
      index.tsx             # redirect → /stats
      stats.tsx
      settings.tsx
      tokens.tsx
      keys.tsx
      nodes.tsx
      logs.tsx
      playground.tsx
  features/
    auth/
    shell/
    stats|settings|tokens|keys|nodes|logs|playground/
  lib/                      # api, query-client, query-keys, constants
  components/ui/            # Cobalt-wrapped Base UI
  styles/ + tokens.css
```

### Ownership rules

- **Auth session** → React context + `localStorage` (not Query); inject into router context for `beforeLoad`
- **Server lists** → per-resource Query keys
- **One-shot UI** → React state (`newToken`, playground result/status/err)
- **`playToken` / `PLAY_TOKEN_KEY`** → survives logout
- **Logout / 401** → clear session keys → `queryClient.clear()` → `router.invalidate()` → `/login` (never `resetQueries` on session end)
- **Layout** does not fan-out all six GETs
- **Guards are UI-only**; API still authorizes Bearer / `adm-`

### Deploy / CI / Docker contract (no dual path)

**One produce-dist path for local, CI, and Docker.** Vite+ is not laptop-only.

1. **`apps/admin/package.json` is SoT** for how `dist/` is built.
   - After migrate, scripts use local `vite-plus` bin (PATH via `node_modules/.bin`):
     - `"dev": "vp dev"`
     - `"build": "tsc -b && vp build"`
     - `"typecheck": "tsc -b"`
     - `"check": "vp check"`
     - `"preview": "vp preview"`
   - Docker does **not** require global curl installer / `setup-vp` if `vite-plus` is a **devDependency** and `npm run build` invokes local `vp`.

2. **Dockerfile `admin-build` (in-scope same program)**  
   Stay on `node:22-bookworm`. Still:
   ```dockerfile
   COPY package.json package-lock.json ./
   RUN npm ci
   COPY . ./
   RUN npm run build && mkdir -p /admin-dist && cp -a dist/. /admin-dist/
   ```
   - Lockfile must include `vite-plus`, `@voidzero-dev/vite-plus-core`, and **npm `overrides`** so `vite` → core (single instance).
   - First green smoke: `/admin/` 200; assets under `/admin/assets/…`.
   - If package scripts change, Docker changes in the **same** change set.

3. **CI `admin` job (in-scope same program)**  
   - Node 22 (`setup-node` and/or `voidzero-dev/setup-vp@v1` for cache — either fine)
   - **Required:** `npm ci` + `npm run build` in `apps/admin` (proves Docker path)
   - **Required:** typecheck via `build` (`tsc -b`) or explicit `npm run typecheck`
   - **Recommended:** `npm run check`
   - Do not teach CI about `vp` while Docker still runs old plain `vite build`.

4. **Docs (same program)**  
   `docs/ops/deploy.md`, `docs/ops/env.md`, README/AGENTS admin commands: Vite+ script names; image still bakes via `npm run build`.

5. **Non-goal**  
   No long-lived dual path (“local vp, CI/Docker plain vite”).

### Vite+ migration notes

- Prefer `vp migrate` or manual `vite-plus` + core + overrides
- Config: `defineConfig` from `vite-plus`; keep `base`, `plugins`, `server.proxy`
- Plugin order: **`tanstackRouter()` before `react()`**
- `vp build` ≠ `vp run build` — use built-ins for check/build; `vp run` only for custom scripts
- Keep explicit **`tsc -b`** in addition to `vp check` until tsgolint is trusted as full strict gate
- Pin `vite-plus` (beta pre-1.0); smoke `/admin/` asset URLs after first build

### Explicit non-goals (foundation)

- No new admin REST/MCP endpoints
- No CF Workers / Nitro / TanStack Start SSR for admin
- No Tailwind / full shadcn / Sonner / `cmdk` package
- No `@tanstack/react-router-ssr-query`
- No dual JS sources or mega `useAdminData` in end state

---

## §2 Routing & auth

### URL map (browser)

| Browser URL | File route | Access |
|---|---|---|
| `/admin/login` | `routes/login.tsx` | Public |
| `/admin/` | `_auth/` index | Authed → **redirect to `/stats`** |
| `/admin/stats` | `_auth/stats.tsx` | Authed |
| `/admin/settings` | `_auth/settings.tsx` | Authed |
| `/admin/tokens` | `_auth/tokens.tsx` | Authed |
| `/admin/keys` | `_auth/keys.tsx` | Authed |
| `/admin/nodes` | `_auth/nodes.tsx` | Authed |
| `/admin/logs` | `_auth/logs.tsx` | Authed |
| `/admin/playground` | `_auth/playground.tsx` | Authed |

Unknown authed paths: soft redirect to `/stats`.

### Route tree

```
__root.tsx                 # createRootRouteWithContext<{ auth; queryClient }>
login.tsx
_auth.tsx                  # pathless: beforeLoad + Shell + <Outlet />
_auth/index.tsx            # → /stats
_auth/{stats,settings,tokens,keys,nodes,logs,playground}.tsx
```

File-based via `@tanstack/router-plugin` (`autoCodeSplitting: true`).

### Auth ownership (`AuthProvider` ← `useAdminSession`)

| Concern | Behavior (preserve wire) |
|---|---|
| Hydration | `SESSION_KEY` preferred over `SECRET_KEY`; load `SESSION_EXPIRES_KEY` |
| `isAuthenticated` | Boolean(token) |
| `applySecretToken` | Store secret; clear session keys |
| `applySessionToken` | Store `adm-` + expires; clear secret |
| Password login | `POST /api/admin/login` → `{ token, expiresAt }` (caller applies) |
| Bootstrap | `POST /api/admin/bootstrap` (Bearer admin secret) then login |
| Logout | Best-effort `POST /api/admin/logout` if session → clear **auth keys only** |
| clearAuth | Drop `SECRET_KEY` / `SESSION_KEY` / `SESSION_EXPIRES_KEY` |

Not in Auth: admin list data; playground `tok-`.

### Provider wiring

```
QueryClientProvider
  └─ AuthProvider
       └─ InnerApp (useAuth + useQueryClient)
            └─ RouterProvider context={{ auth, queryClient }}
```

- Module-scope `createRouter({ context: { auth: undefined!, queryClient }, basepath: '/admin' })`
- Live auth only from `InnerApp` (hooks illegal in `beforeLoad`)

### Guards

**`_auth` `beforeLoad`:** if `!context.auth.isAuthenticated`, throw redirect to `/login` with **path-only** `search.redirect` (see below).

**`login` `beforeLoad`:** if already authed → safe `redirect` target or `/stats`.

**`redirect` search (anti open-redirect):**
- When guarding, pass a **path-only** router path such as `/keys` or `/playground` (from the matched location’s path **without** origin and **without** re-adding `/admin` — basepath handles prefixing).
- **Never** put full `location.href`, absolute `https://…`, or protocol-relative URLs in `search.redirect`.
- Shared helper (e.g. `safeRedirectPath(value): '/stats' | panel path`) allowlists panel paths (or matches `^/([a-z0-9-]+)?$` against known sections); invalid → `/stats`.
- Post-login / bootstrap / secret: apply token → `await router.invalidate()` → `navigate({ to: safeRedirectPath(search.redirect) })`.

**Secret mode:** apply → invalidate → navigate; validation via first panel query + global 401.

### Login UI

`/login` page with three modes: ADMIN_SECRET | Password | Bootstrap. Same API bodies/headers. Typed `validateSearch` for optional `redirect` string; always run through `safeRedirectPath` before navigate.

### Shell navigation

- Sidebar: `Link` per panel; Router active state; ids from `SECTIONS`
- Topbar: refresh (active panel only), session expiry, logout, open CmdK
- CmdK: Base UI Dialog + Autocomplete; select → `navigate({ to: \`/${id}\` })` (no scroll)

### Logout & 401

| Event | Steps |
|---|---|
| Logout | best-effort server logout → clearAuth → `queryClient.clear()` → invalidate → `/login` |
| Global 401 | same; dedupe in-flight |
| Never | clear `PLAY_TOKEN_KEY`; `resetQueries` on logout |

### Non-goals (§2)

- No cookie redesign; still Bearer
- No server-side route auth
- No hash-section hybrid

---

## §3 Data layer (TanStack Query)

### Kill the mega-hook

Replace `useAdminData` control plane with per-resource Query, Auth for bearer, feature state for one-shots, Toast for feedback.

### QueryClient defaults

```ts
defaultOptions: {
  queries: {
    staleTime: 30_000,
    gcTime: 5 * 60_000,
    retry: (n, err) => !isUnauthorized(err) && n < 2,
    refetchOnWindowFocus: true,
  },
  mutations: { retry: false },
}
```

| Resource | staleTime |
|---|---|
| stats | ~10s |
| settings | 60s |
| tokens / keys / nodes | 30s default |
| request-logs | `0` |
| playground | not a query |

Stable client: singleton or one `useState(() => new QueryClient())` at root.

### Key factory

Hierarchical: `qk.stats.all`, `qk.tokens.list()`, `qk.keys.*`, `qk.settings.*`, `qk.nodes.*`, `qk.requestLogs.list({ limit: 50 })`.  
No session query key — identity stays in Auth.

### Queries (per route)

| Route | Endpoint |
|---|---|
| `/stats` | `GET /api/stats` |
| `/settings` | `GET /api/settings` |
| `/tokens` | `GET /api/tokens` |
| `/keys` | `GET /api/keys` |
| `/nodes` | `GET /api/nodes` |
| `/logs` | `GET /api/request-logs?limit=50` |
| `/playground` | product `fetch` with playToken — not admin list queries |

Layout does not prefetch all six. Optional shared stats query for topbar schema chip.

`queryFn` uses `getAdminBearer()` (session then secret). Typed `adminFetch<T>`; errors carry `.status`.

### Mutations + invalidation

| Mutation | API | Invalidate |
|---|---|---|
| createToken | `POST /api/tokens` | `qk.tokens.all` + local `newToken` |
| deleteToken | `DELETE /api/tokens/:id` | `qk.tokens.all` |
| createKey | `POST /api/keys` | `qk.keys.all` (+ stats if counts) |
| toggleKey | `POST /api/keys/:id/toggle` | `qk.keys.all` |
| deleteKey | `DELETE /api/keys/:id` | `qk.keys.all` |
| syncCredits | `POST /api/keys/sync-credits` | `qk.keys.all`; honesty toast from report |
| saveSettings | `PUT /api/settings` | `qk.settings.all` |
| createNode | `POST /api/nodes` | `qk.nodes.all` |
| toggleNode | `POST /api/nodes/:id/toggle` | `qk.nodes.all` |
| deleteNode | `DELETE /api/nodes/:id` | `qk.nodes.all` |
| logs refresh | refetch logs query | — |

Return invalidate promises from `onSuccess`. Destructive confirms → Base UI AlertDialog.  
Topbar Refresh → invalidate **active panel prefix only**.

### Client-only state

| State | Owner | Logout |
|---|---|---|
| newToken | Tokens feature state | drops |
| playToken | `PLAY_TOKEN_KEY` + state | **survives** |
| playResult / playStatus / playErr | Playground state | drops |
| Form drafts / CmdK | local | drops |

### Feedback

- MutationCache + optional `meta.successMessage` / `errorMessage` → Toast
- Credit-sync partial: preserve `synced`/`errors`/failed-id copy
- Query errors: per-panel region; login errors stay on `/login`

### Global 401

QueryCache + MutationCache `onError`: if 401 → clearAuth → `queryClient.clear()` → login. Skip retry on 401. Dedupe. Do not touch `PLAY_TOKEN_KEY`.

### Playground

Product-token fetch for search/extract/research paths as today. Status chip on errors preserved.

### Non-goals (§3)

- No required optimistic updates in v1
- No Query persistence plugin
- No required route loaders / `ensureQueryData`
- No blanket invalidate-all on CRUD

---

## §4 UI chrome, Base UI, panel modernize

### Visual SoT

- `design.md` + `apps/admin/tokens.css` (Cobalt)
- No Tailwind; Base UI via `className` + `data-*` / CSS vars
- Evolve `styles.css` (optional light split: shell / panels / overlays)
- Portal root: `.root { isolation: isolate }`

### Shell

```
Topbar: wordmark · schema/exp chips · ⌘K · Refresh · Logout
Sidebar (Links) | <Outlet /> one panel
Colophon
```

- Drop global mega-`busy` chip; per-mutation `isPending` on actions
- Toasts primary; panels keep inline load errors
- `/login` full page, not overlay
- Narrow viewports: sidebar → drawer/dialog

### Base UI kit (`components/ui/`)

| Primitive | Use |
|---|---|
| Dialog | CmdK host; occasional modals |
| AlertDialog | Delete confirms |
| Menu | Row action overflow (optional polish) |
| Toast | Global feedback |
| Autocomplete + Dialog | CmdK |
| Combobox | Optional filterable selects only if it improves an existing form |

Do **not** add Sonner, cmdk, Radix, full shadcn.

### SPA-only UX (in scope)

- Toast stack
- AlertDialog deletes
- Per-panel loading / empty
- Client list filter (tokens, keys, nodes, logs)
- Disable row actions while pending
- Sidebar active route

### Panels

Same capabilities and API shapes; better presentation only (stats strip, tables, forms panel-owned, playground modes + honesty).

### A11y / motion

- Focus trap + Dialog.Close on modals
- `prefers-reduced-motion` per design.md
- Practical 44px primary controls

### Non-goals (§4)

- No Cobalt abandonment / full rebrand
- No new product panels
- No chart library
- No required virtualization

---

## §5 Errors, testing, rollout

### Error layers

| Layer | Behavior |
|---|---|
| HTTP parse | problem+json → Error + `.status` |
| 401 | global clear + login |
| Mutation 4xx/5xx | toast; panel stays |
| Query fail | panel error + refetch |
| Login | inline on `/login` |
| Credit sync partial | honesty toast/err |
| Playground | playErr + status chip (playToken path) |
| Render throws | light root/auth error UI |

### Testing

| Gate | What |
|---|---|
| Typecheck | `tsc -b` |
| Vite+ check | `vp check` / `npm run check` |
| Build | `npm run build` → `/admin/`-prefixed assets |
| CI admin | install + typecheck/check + build |
| Docker | existing smoke; SPA stage uses same `npm run build` |
| Manual smoke | login modes; each panel; CRUD sample; CmdK; logout; 401; playToken survives |

No Vitest/Playwright required for v1.

### Implementation order (green commits)

1. **Vite+ foundation** + scripts + plugin-react v6 + **CI admin + Dockerfile admin-build** same contract  
2. **Strict TS scaffold** (triple tsconfig, entry rename, path to zero JS end state)  
3. **lib** — api, constants, query-client, query-keys, AuthProvider  
4. **Router shell** — plugin, routes, basepath, login + `_auth`, index→stats  
5. **Query per panel** — port features; delete `useAdminData` when done  
6. **Base UI chrome** — toast, AlertDialog, sidebar, CmdK  
7. **Panel modernize** — loading/empty/filter  
8. **Docs** — deploy/env/README/AGENTS  
9. **Final gate** — typecheck, check, build, CI, Docker smoke, manual checklist  

### Cutover

- `localStorage` key names unchanged — sessions survive deploy  
- `/admin/` → stats redirect; new deep links work  
- No feature-flag dual SPA  
- Rollback = previous image tag  

### Success criteria

1. Strict TS; no `src/**/*.{js,jsx}`  
2. Vite+ produces `dist/` for local, CI, and Docker via `npm run build`  
3. Real panel routes; `/login` + guards; CmdK navigates  
4. Per-resource Query; no mega-refresh; logout/401 `clear()`; playToken survives  
5. Cobalt + Base UI; toasts + confirms; same admin APIs only  
6. CI admin + Docker admin-build green; assets under `/admin/`  

---

## Research basis (librarians)

| Topic | Pin |
|---|---|
| Router | `@tanstack/react-router` ~1.170; plugin before react; `basepath: '/admin'` |
| Query | `@tanstack/react-query` ~5.101; no SSR bridge package |
| TS | Vite-style triple tsconfig; `tsc -b &&` build; no Zod |
| Base UI | `@base-ui/react` ~1.6; Toast first-class; CmdK = Autocomplete+Dialog |
| SPA patterns | Auth context + router context; hybrid routes/ + features/; logout `clear()` |
| Vite+ | `vite-plus` ~0.2.x beta; Vite 8 + Rolldown; overrides required; `base` still standard |

---

## Out of scope (program)

- Backend/admin API changes  
- MCP/tooling rebrand in SPA beyond existing playground  
- Vite+ monorepo-wide adoption outside `apps/admin`  
- Dark theme, i18n, multi-admin RBAC UI  

## Next step

Implementation plan via `writing-plans` against this spec (`docs/superpowers/plans/2026-07-29-admin-spa-tanstack-viteplus.md`).
