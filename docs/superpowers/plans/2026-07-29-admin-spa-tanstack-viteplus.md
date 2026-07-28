# Admin SPA TanStack + Vite+ Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:dispatching-parallel-agents for independent tasks to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rebuild `apps/admin` as a strict TypeScript SPA on Vite+ with file-based TanStack Router, per-resource TanStack Query, Base UI + Cobalt chrome, and one shared `npm run build` path for local/CI/Docker.

**Architecture:** Vite+ (Vite 8 + Rolldown) is the toolchain foundation. Auth lives in React context injected into Router; pathless `_auth` layout guards panel routes. Each panel owns its Query keys/mutations. Base UI wraps Dialog/AlertDialog/Toast/CmdK; Cobalt tokens stay SoT. No new backend endpoints.

**Tech Stack:** `vite-plus` + `@voidzero-dev/vite-plus-core`, React 19, `@vitejs/plugin-react@^6`, `@tanstack/react-router` + `@tanstack/router-plugin`, `@tanstack/react-query` v5, `@base-ui/react`, TypeScript strict, Cobalt CSS.

**Spec:** `docs/superpowers/specs/2026-07-29-admin-spa-tanstack-viteplus-design.md`

## Global Constraints

- **Backend frozen** — no new admin REST/MCP endpoints; preserve all current paths/bodies/headers
- **Vite `base: '/admin/'`** + Router **`basepath: '/admin'`** — route `to` values unprefixed (`/login`, `/stats`)
- **One produce-dist path** — local, CI admin job, Dockerfile `admin-build` all use `npm run build` (Vite+ under the hood); no dual “vp laptop / plain vite image”
- **Strict TS end state** — zero `src/**/*.{js,jsx}`
- **Auth in context**, not Query; **playToken / `PLAY_TOKEN_KEY` survives logout**
- **Logout/401** → clearAuth + `queryClient.clear()` + `/login` — never `resetQueries` on session end
- **`redirect` search path-only** via `safeRedirectPath` — never full `location.href` (open-redirect)
- **No Zod**, no Tailwind, no Sonner/cmdk/Radix, no SSR Query package, no optimistic toggles v1, no Vitest/Playwright required
- **Never** `git commit --no-verify`
- Prefer `rtk` wrappers for git/npm where available
- Pin versions near research: `vite-plus` ~0.2.x, router ~1.170, query ~5.101, base-ui ~1.6

## File map (end state)

| Path | Responsibility |
| --- | --- |
| `apps/admin/package.json` | deps, overrides (`vite` → core), scripts `dev/build/typecheck/check/preview` |
| `apps/admin/package-lock.json` | lock after migrate |
| `apps/admin/vite.config.ts` | `defineConfig` from `vite-plus`; base; tanstackRouter before react; proxy; optional lint/fmt/check |
| `apps/admin/tsconfig.json` | project references |
| `apps/admin/tsconfig.app.json` | strict app TS |
| `apps/admin/tsconfig.node.json` | vite.config.ts |
| `apps/admin/index.html` | entry → `/src/main.tsx` |
| `apps/admin/src/main.tsx` | providers + RouterProvider |
| `apps/admin/src/router.tsx` | createRouter + Register + basepath |
| `apps/admin/src/routeTree.gen.ts` | plugin-generated (do not hand-edit) |
| `apps/admin/src/vite-env.d.ts` | vite/client + `VITE_API_BASE` |
| `apps/admin/src/lib/constants.ts` | storage keys + SECTIONS |
| `apps/admin/src/lib/api.ts` | `apiBase`, `parseJsonResponse`, `adminFetch<T>`, `getAdminBearer` |
| `apps/admin/src/lib/query-keys.ts` | `qk.*` factory |
| `apps/admin/src/lib/query-client.ts` | QueryClient + 401 handlers factory |
| `apps/admin/src/lib/safe-redirect.ts` | `safeRedirectPath` |
| `apps/admin/src/features/auth/*` | AuthProvider, types, login page UI |
| `apps/admin/src/features/shell/*` | Shell, Topbar, Sidebar, CmdK |
| `apps/admin/src/features/{stats,settings,tokens,keys,nodes,logs,playground}/*` | panel + queries/mutations |
| `apps/admin/src/components/ui/*` | Cobalt-wrapped Base UI (Dialog, AlertDialog, Toast, etc.) |
| `apps/admin/src/routes/*` | thin file routes |
| `apps/admin/tokens.css` | keep SoT |
| `apps/admin/src/styles.css` (or `styles/*`) | evolved Cobalt shell/panels/overlays |
| `Dockerfile` | `admin-build` still `npm ci` + `npm run build` (scripts change underneath) |
| `.github/workflows/ci.yml` | admin job: typecheck/check + build |
| `docs/ops/deploy.md`, `docs/ops/env.md`, `README.md`, `AGENTS.md` | SPA build command docs |

**Delete by end:** `App.jsx`, `main.jsx`, `api.js`, `constants.js`, hooks `useAdminData.js` / `useAdminSession.js` / `useCmdk.js`, old `components/*` JS panels once ported, `vite.config.js`.

**Serial dependency:** Tasks 1 → 2 → 3 → 4 are sequential foundations. Tasks 5–11 (panels) can proceed panel-by-panel after Task 4. Task 12 chrome can overlap late panels. Task 13 docs last.

---


### Task 1: Vite+ foundation + shared build contract

**Files:**
- Modify: `apps/admin/package.json`
- Modify: `apps/admin/package-lock.json` (via npm)
- Create: `apps/admin/vite.config.ts`
- Delete: `apps/admin/vite.config.js` (after TS config works)
- Modify: `Dockerfile` (only if build command text must change — prefer keeping `npm run build`)
- Modify: `.github/workflows/ci.yml` admin job steps

**Interfaces:**
- Consumes: existing `base: '/admin/'`, proxy `/api|/live|/ready`
- Produces: `npm run build` → `apps/admin/dist/` with assets under `/admin/`; scripts callable in Docker without global `vp`

- [ ] **Step 1: Install Vite+ and React plugin v6 in `apps/admin`**

From `apps/admin`:

```bash
cd apps/admin
npm install -D vite-plus @voidzero-dev/vite-plus-core@latest
npm install -D @vitejs/plugin-react@^6
# remove direct vite@6 if still listed — overrides alias vite → core
```

Ensure `package.json` has:

```json
"overrides": {
  "vite": "npm:@voidzero-dev/vite-plus-core@latest"
}
```

Optional: `vp migrate` non-interactive, then re-check overrides + scripts match Step 2.

- [ ] **Step 2: Rewrite scripts (Vite+ only — no tsc yet)**

```json
"scripts": {
  "dev": "vp dev",
  "build": "vp build",
  "preview": "vp preview"
}
```

Keep `"type": "module"`. **Do not** add `typecheck` / `tsc -b` / `vp check` here — **Task 2** owns typecheck; optional `check` can land with Task 2 or later.

Runtime: `react` / `react-dom` ^19.  
DevDeps this task: `vite-plus`, `@voidzero-dev/vite-plus-core`, `@vitejs/plugin-react@^6` only.

Vite+ engines: Node **`^20.19.0 || ^22.18.0 || >=24.11.0`**. Pin CI + Docker to **22.18+** in Step 5.

- [ ] **Step 3: Replace config with `vite.config.ts`**

```ts
import path from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite-plus";
import react from "@vitejs/plugin-react";

const root = path.dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  base: "/admin/",
  plugins: [react()],
  resolve: {
    alias: { "@": path.resolve(root, "src") },
  },
  server: {
    port: 5173,
    proxy: {
      "/api": "http://127.0.0.1:8080",
      "/live": "http://127.0.0.1:8080",
      "/ready": "http://127.0.0.1:8080",
    },
  },
});
```

If `@types/node` is missing and the editor complains, either leave alias out until Task 2 installs `@types/node`, or add a minimal `// @ts-nocheck` is **not** allowed — prefer installing `@types/node` early **only if** required to parse config; otherwise keep JS-free path via Task 2.

Practical path: install `@types/node` in Task 1 **only as** `npm i -D @types/node` so `vite.config.ts` typechecks when tsc arrives — still no full typescript app stack yet.

```bash
npm install -D @types/node
```

Delete `vite.config.js` once `vp build` uses `vite.config.ts`. **No** TanStack router plugin yet (Task 4).

**Note:** `vite.config.ts` without project `typescript` may still run under Vite's esbuild transpile. Task 2 adds `typescript` + `tsc -b`.

- [ ] **Step 4: Prove local dist**

```bash
cd apps/admin && npm run build
rg -n '/admin/' apps/admin/dist/index.html
```

Expected: exit 0; asset hrefs start with `/admin/`.

- [ ] **Step 5: Pin Node 22.18+ in CI + Docker (real Task 1 CI change)**

CI already runs `npm run build` — the **substantive** CI change is the Node version pin so Vite+ engines are satisfied:

`.github/workflows/ci.yml` admin job:

```yaml
- uses: actions/setup-node@v4
  with:
    node-version: "22.18"
    cache: npm
    cache-dependency-path: apps/admin/package-lock.json
- run: npm ci
- run: npm run build
```

Dockerfile `admin-build`:

```dockerfile
FROM node:22.18-bookworm AS admin-build
```

(or newer 22.x bookworm that is ≥22.18). Keep:

```dockerfile
RUN npm ci
COPY apps/admin/ ./
RUN npm run build \
    && mkdir -p /admin-dist \
    && cp -a dist/. /admin-dist/
```

No global `curl | bash` Vite+ install in Docker. Optional `package.json`:

```json
"engines": { "node": "^22.18.0 || >=24.11.0" }
```

- [ ] **Step 6: Commit**

```bash
rtk git add apps/admin/package.json apps/admin/package-lock.json apps/admin/vite.config.ts
rtk git add -u apps/admin/vite.config.js
rtk git add .github/workflows/ci.yml Dockerfile
rtk git commit -m "build(admin): adopt Vite+ shared npm build"
```

---

### Task 2: Strict TypeScript scaffold

**Files:**
- Create: `apps/admin/tsconfig.json`, `tsconfig.app.json`, `tsconfig.node.json`
- Create: `apps/admin/src/vite-env.d.ts`
- Modify: `apps/admin/index.html` (script → `/src/main.tsx`)
- Create: `apps/admin/src/main.tsx` (temporary bridge to existing App)
- Delete: `apps/admin/src/main.jsx` after switch
- Modify: `apps/admin/package.json` — add TS deps + `typecheck` + `build: tsc -b && vp build`

**Interfaces:**
- Consumes: Vite+ config from Task 1
- Produces: `npm run typecheck` and `npm run build` both green with `allowJs: true`

- [ ] **Step 1: Install TypeScript toolchain**

```bash
cd apps/admin
npm install -D typescript@~5.8 @types/react@^19 @types/react-dom@^19 @types/node
```

(`@types/node` may already exist from Task 1.)

- [ ] **Step 2: Add triple tsconfig**

`tsconfig.json`:

```json
{
  "files": [],
  "references": [
    { "path": "./tsconfig.app.json" },
    { "path": "./tsconfig.node.json" }
  ]
}
```

`tsconfig.app.json`:

```json
{
  "compilerOptions": {
    "tsBuildInfoFile": "./node_modules/.tmp/tsconfig.app.tsbuildinfo",
    "target": "ES2020",
    "useDefineForClassFields": true,
    "lib": ["ES2020", "DOM", "DOM.Iterable"],
    "module": "ESNext",
    "skipLibCheck": true,
    "moduleResolution": "bundler",
    "allowImportingTsExtensions": true,
    "verbatimModuleSyntax": true,
    "moduleDetection": "force",
    "noEmit": true,
    "jsx": "react-jsx",
    "strict": true,
    "noUnusedLocals": true,
    "noUnusedParameters": true,
    "noFallthroughCasesInSwitch": true,
    "baseUrl": ".",
    "paths": { "@/*": ["src/*"] },
    "allowJs": true
  },
  "include": ["src"]
}
```

`allowJs: true` for incremental port; **final gate** (Task 13) sets `allowJs: false` and zero JS under `src/`.

`tsconfig.node.json`:

```json
{
  "compilerOptions": {
    "tsBuildInfoFile": "./node_modules/.tmp/tsconfig.node.tsbuildinfo",
    "target": "ES2022",
    "lib": ["ES2023"],
    "module": "ESNext",
    "skipLibCheck": true,
    "moduleResolution": "bundler",
    "allowImportingTsExtensions": true,
    "verbatimModuleSyntax": true,
    "moduleDetection": "force",
    "noEmit": true,
    "strict": true,
    "noUnusedLocals": true,
    "noUnusedParameters": true,
    "noFallthroughCasesInSwitch": true,
    "types": ["node"]
  },
  "include": ["vite.config.ts"]
}
```

- [ ] **Step 3: `src/vite-env.d.ts`**

```ts
/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_API_BASE?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
```

- [ ] **Step 4: Point HTML at `main.tsx`; remove `main.jsx`**

`index.html`:

```html
<script type="module" src="/src/main.tsx"></script>
```

`src/main.tsx`:

```tsx
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App.jsx";
import "./styles.css";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
```

- [ ] **Step 5: Scripts — Task 2 is the only place that adds tsc to build**

```json
"scripts": {
  "dev": "vp dev",
  "typecheck": "tsc -b --pretty false",
  "check": "vp check",
  "build": "tsc -b && vp build",
  "preview": "vp preview"
}
```

- [ ] **Step 6: Gate**

```bash
cd apps/admin && npm run typecheck && npm run build
```

Expected: both exit 0; dist still `/admin/`-prefixed.

- [ ] **Step 7: Commit**

```bash
rtk git add apps/admin/tsconfig.json apps/admin/tsconfig.app.json apps/admin/tsconfig.node.json \
  apps/admin/src/vite-env.d.ts apps/admin/src/main.tsx apps/admin/index.html \
  apps/admin/package.json apps/admin/package-lock.json
rtk git add -u apps/admin/src/main.jsx
rtk git commit -m "build(admin): strict TypeScript scaffold"
```

---


### Task 3: lib — api, constants, query-keys, safe-redirect, AuthProvider

**Files:**
- Create: `apps/admin/src/lib/constants.ts`
- Create: `apps/admin/src/lib/api.ts`
- Create: `apps/admin/src/lib/query-keys.ts`
- Create: `apps/admin/src/lib/safe-redirect.ts`
- Create: `apps/admin/src/lib/query-client.ts` (factory; wire 401 navigate in Task 4 once router exists)
- Create: `apps/admin/src/features/auth/auth-context.tsx` (AuthProvider + `useAuth`)
- Create: `apps/admin/src/features/auth/types.ts`
- Keep old JS hooks working until Task 4 cuts over (or dual-import carefully)

**Interfaces:**
- Consumes: storage key names from current `constants.js`; HTTP shapes from `useAdminSession` / `api.js`
- Produces:

```ts
// constants
export const SECRET_KEY = "serpotter_admin_secret";
export const SESSION_KEY = "serpotter_admin_session";
export const SESSION_EXPIRES_KEY = "serpotter_admin_session_expires";
export const PLAY_TOKEN_KEY = "serpotter_play_token";
export const SECTIONS: readonly { id: SectionId; label: string }[];
export type SectionId =
  | "stats" | "settings" | "tokens" | "keys" | "nodes" | "logs" | "playground";

// api
export function apiBase(): string;
export function getAdminBearer(): string | null; // SESSION_KEY then SECRET_KEY
export class HttpError extends Error { status: number }
export async function parseJsonResponse<T>(res: Response): Promise<T>;
export async function adminFetch<T>(path: string, opts?: RequestInit & { bearer?: string | null }): Promise<T>;

// safe-redirect
export function safeRedirectPath(value: unknown): string; // '/stats' | `/${SectionId}`

// query-keys
export const qk: {
  stats: { all: readonly ["stats"]; summary: () => readonly ["stats", "summary"] };
  tokens: { all: readonly ["tokens"]; list: () => readonly ["tokens", "list"] };
  keys: { all: readonly ["keys"]; list: () => readonly ["keys", "list"] };
  settings: { all: readonly ["settings"]; root: () => readonly ["settings", "root"] };
  nodes: { all: readonly ["nodes"]; list: () => readonly ["nodes", "list"] };
  requestLogs: {
    all: readonly ["request-logs"];
    list: (f?: { limit?: number }) => readonly ["request-logs", "list", { limit: number }];
  };
};

// auth
export type AuthContextValue = {
  token: string;
  sessionExpiresAt: string;
  isAuthenticated: boolean;
  busy: boolean;
  err: string;
  setErr: (s: string) => void;
  applySecretToken: (s: string) => void;
  applySessionToken: (token: string, expiresAt?: string) => void;
  clearAuth: () => void;
  logout: () => void;
  loginWithPasswordHttp: (p: { username: string; password: string }) => Promise<{ token: string; expiresAt: string }>;
  bootstrapHttp: (p: { adminSecret: string; loginUser: string; password: string }) => Promise<{ token: string; expiresAt: string }>;
};
export function AuthProvider(props: { children: React.ReactNode }): JSX.Element;
export function useAuth(): AuthContextValue;
```

- [ ] **Step 1: Port `constants.ts` and `safe-redirect.ts`**

`SECTIONS` ids/labels unchanged from `constants.js`.

```ts
// safe-redirect.ts
import { SECTIONS, type SectionId } from "./constants";

const ALLOWED = new Set<string>([
  "/stats",
  ...SECTIONS.map((s) => `/${s.id}`),
]);

/** Path-only post-login target. Never accepts absolute URLs. */
export function safeRedirectPath(value: unknown): string {
  if (typeof value !== "string") return "/stats";
  const raw = value.trim();
  if (!raw.startsWith("/") || raw.startsWith("//")) return "/stats";
  // strip optional accidental /admin prefix if someone stored browser path
  const path = raw.startsWith("/admin/")
    ? raw.slice("/admin".length) || "/"
    : raw === "/admin"
      ? "/"
      : raw;
  const noQuery = path.split("?")[0]?.split("#")[0] ?? "/";
  if (noQuery === "/" || noQuery === "") return "/stats";
  if (ALLOWED.has(noQuery)) return noQuery;
  // single segment /stats style
  const m = /^\/([a-z0-9-]+)$/.exec(noQuery);
  if (m && SECTIONS.some((s) => s.id === m[1])) return `/${m[1] as SectionId}`;
  return "/stats";
}
```

- [ ] **Step 2: Port `api.ts`**

Preserve Bearer preference: `localStorage SESSION_KEY` then explicit/opts bearer then secret. Throw `HttpError` with `.status` on non-OK (so Query 401 works). Prefer reading bearer inside `adminFetch` via `getAdminBearer()` when `opts.bearer` omitted.

```ts
export function getAdminBearer(): string | null {
  if (typeof localStorage === "undefined") return null;
  return (
    localStorage.getItem(SESSION_KEY) ||
    localStorage.getItem(SECRET_KEY) ||
    null
  );
}
```

Map problem+json `detail`/`title` like today.

- [ ] **Step 3: `query-keys.ts` exactly as Interfaces block**

- [ ] **Step 4: `query-client.ts` factory**

```ts
export function createAppQueryClient(handlers: {
  onUnauthorized: () => void;
}): QueryClient {
  let handling401 = false;
  const handle401 = () => {
    if (handling401) return;
    handling401 = true;
    try {
      handlers.onUnauthorized();
    } finally {
      queueMicrotask(() => {
        handling401 = false;
      });
    }
  };
  const isUnauthorized = (e: unknown) =>
    typeof e === "object" &&
    e !== null &&
    "status" in e &&
    (e as { status: number }).status === 401;

  return new QueryClient({
    queryCache: new QueryCache({
      onError: (err) => {
        if (isUnauthorized(err)) handle401();
      },
    }),
    mutationCache: new MutationCache({
      onError: (err) => {
        if (isUnauthorized(err)) handle401();
      },
    }),
    defaultOptions: {
      queries: {
        staleTime: 30_000,
        gcTime: 5 * 60_000,
        retry: (n, err) => !isUnauthorized(err) && n < 2,
        refetchOnWindowFocus: true,
      },
      mutations: { retry: false },
    },
  });
}
```

Install `@tanstack/react-query@^5` now if not already.

- [ ] **Step 5: AuthProvider port from `useAdminSession.js`**

Same storage semantics, login/bootstrap/logout HTTP. `token` state replaces loosely named `secret`. `isAuthenticated = Boolean(token)`.

`logout`: best-effort `POST /api/admin/logout` with session bearer only; then `clearAuth` — **do not** touch `PLAY_TOKEN_KEY`.

- [ ] **Step 6: Gate**

```bash
cd apps/admin && npm run typecheck
```

Expected: new TS files clean (allowJs still on for old JSX).

- [ ] **Step 7: Commit**

```bash
rtk git add apps/admin/src/lib apps/admin/src/features/auth apps/admin/package.json apps/admin/package-lock.json
rtk git commit -m "feat(admin): typed api auth lib and query keys"
```

---

### Task 4: Router shell — login, _auth guard, basepath

**Files:**
- Modify: `apps/admin/vite.config.ts` — add `tanstackRouter` **before** `react()`
- Modify: `apps/admin/package.json` — add `@tanstack/react-router`, `@tanstack/router-plugin`, devtools optional
- Create: `apps/admin/src/router.tsx`
- Create: `apps/admin/src/routes/__root.tsx`
- Create: `apps/admin/src/routes/login.tsx`
- Create: `apps/admin/src/routes/_auth.tsx`
- Create: `apps/admin/src/routes/_auth/index.tsx`
- Create: `apps/admin/src/routes/_auth/stats.tsx` (minimal placeholder OK)
- Create: other `_auth/*.tsx` stubs that render “Coming soon” **or** wait until panel tasks — prefer stubs so route tree is complete
- Create: `apps/admin/src/features/auth/LoginPage.tsx` (port LoginGate UI)
- Modify: `apps/admin/src/main.tsx` — QueryClientProvider → AuthProvider → InnerApp → RouterProvider
- Delete: old `App.jsx` composition path once shell works

**Interfaces:**
- Consumes: `AuthProvider`, `useAuth`, `createAppQueryClient`, `safeRedirectPath`, `SECTIONS`
- Produces: navigable `/admin/login`, `/admin/stats`, … with guard

- [ ] **Step 1: Install router packages**

```bash
cd apps/admin
npm install @tanstack/react-router
npm install -D @tanstack/router-plugin @tanstack/react-router-devtools
```

- [ ] **Step 2: vite plugin order**

```ts
import { tanstackRouter } from "@tanstack/router-plugin/vite";

plugins: [
  tanstackRouter({ target: "react", autoCodeSplitting: true }),
  react(),
],
```

- [ ] **Step 3: `router.tsx`**

```tsx
import { createRouter } from "@tanstack/react-router";
import { routeTree } from "./routeTree.gen";
import type { AuthContextValue } from "./features/auth/types";
import type { QueryClient } from "@tanstack/react-query";

export type RouterContext = {
  auth: AuthContextValue;
  queryClient: QueryClient;
};

export const router = createRouter({
  routeTree,
  basepath: "/admin",
  defaultPreload: "intent",
  context: {
    auth: undefined!,
    queryClient: undefined!,
  },
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
```

- [ ] **Step 4: Routes**

`__root.tsx`: `createRootRouteWithContext<RouterContext>()({ component: () => <Outlet /> })`.

`login.tsx`:
- `validateSearch`: `{ redirect?: string }`
- `beforeLoad`: if `context.auth.isAuthenticated` → `redirect({ to: safeRedirectPath(search.redirect) })`
- component: `LoginPage`

`_auth.tsx`:

```ts
beforeLoad: ({ context, location }) => {
  if (!context.auth.isAuthenticated) {
    throw redirect({
      to: "/login",
      search: {
        redirect: safeRedirectPath(location.pathname),
      },
    });
  }
},
component: AuthShell, // temporary: Outlet only; full chrome Task 12
```

**Important:** `location.pathname` from Router is already basepath-stripped — pass through `safeRedirectPath` (do **not** use `location.href`).

`_auth/index.tsx`: `beforeLoad` or component `Navigate` → `/stats`.

Stub each panel route file with a heading so Links work.

- [ ] **Step 5: `main.tsx` provider tree**

```tsx
const queryClient = createAppQueryClient({
  onUnauthorized: () => {
    // clearAuth + clear + navigate — implement with router import carefully to avoid cycles:
    // preferred: auth.clearAuth(); queryClient.clear(); void router.navigate({ to: '/login' });
  },
});

function InnerApp() {
  const auth = useAuth();
  return (
    <RouterProvider
      router={router}
      context={{ auth, queryClient }}
    />
  );
}

createRoot(...).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <AuthProvider>
        <InnerApp />
      </AuthProvider>
    </QueryClientProvider>
  </StrictMode>,
);
```

Wire `onUnauthorized` to: read auth clear via a small module callback set from AuthProvider, or clear storage keys directly matching `clearAuth` + `queryClient.clear()` + `router.navigate({ to: '/login' })` + `router.invalidate()`.

- [ ] **Step 6: LoginPage actions**

On secret/password/bootstrap success:
1. `applySecretToken` / `applySessionToken`
2. `await router.invalidate()`
3. `navigate({ to: safeRedirectPath(search.redirect) })`

- [ ] **Step 7: Gate**

```bash
cd apps/admin && npm run typecheck && npm run build
npm run dev
# open http://localhost:5173/admin/login
```

Expected: unauthenticated `/admin/stats` redirects to login with safe redirect search; login navigates to stats stub.

- [ ] **Step 8: Commit**

```bash
rtk git add apps/admin
rtk git commit -m "feat(admin): TanStack Router auth shell and login"
```

---


### Task 5: Stats panel (Query template for later panels)

**Files:**
- Create: `apps/admin/src/features/stats/queries.ts`
- Create: `apps/admin/src/features/stats/StatsPanel.tsx`
- Modify: `apps/admin/src/routes/_auth/stats.tsx` → render `StatsPanel`
- Optional: shared stats query for topbar schema chip (same `qk.stats.summary()`)

**Interfaces:**
- Consumes: `adminFetch`, `qk.stats`, `useQuery`
- Produces: pattern later panels copy

```ts
// queries.ts
export const statsQueryOptions = queryOptions({
  queryKey: qk.stats.summary(),
  queryFn: () => adminFetch<StatsDto>("/api/stats"),
  staleTime: 10_000,
});
```

- [ ] **Step 1: Define `StatsDto` interface** from current stats JSON fields used in UI (at minimum `schemaVersion` and any metrics Topbar/Stats show today). Read current `StatsPanel.jsx` and mirror fields — no invented API fields.

- [ ] **Step 2: Implement `StatsPanel` with `useQuery(statsQueryOptions)`**

States: pending skeleton/spinner; error region + retry (`refetch`); success render (metric strip / definition list — Cobalt, not SaaS card grid).

- [ ] **Step 3: Wire route**

```tsx
// routes/_auth/stats.tsx
import { createFileRoute } from "@tanstack/react-router";
import { StatsPanel } from "@/features/stats/StatsPanel";

export const Route = createFileRoute("/_auth/stats")({
  component: StatsPanel,
});
```

- [ ] **Step 4: Gate**

```bash
cd apps/admin && npm run typecheck && npm run build
```

Manual: login → `/admin/stats` loads data (API up) or shows error (API down) without crashing shell.

- [ ] **Step 5: Commit**

```bash
rtk git add apps/admin/src/features/stats apps/admin/src/routes/_auth/stats.tsx
rtk git commit -m "feat(admin): stats panel with TanStack Query"
```

---

### Task 6: Settings panel

**Files:**
- Create: `apps/admin/src/features/settings/queries.ts` (+ mutations)
- Create: `apps/admin/src/features/settings/SettingsPanel.tsx`
- Modify: `apps/admin/src/routes/_auth/settings.tsx`

**Interfaces:**
- `GET/PUT /api/settings` with body `{ socialEnabled }` — preserve field names from current `saveSettings`
- Invalidate `qk.settings.all` on success; toast success/error (Toast provider may be stub until Task 12 — use `meta` ready for Toast or temporary `console`/inline err until chrome lands)

- [ ] **Step 1: `settingsQueryOptions` + `useSaveSettingsMutation`**

```ts
queryKey: qk.settings.root(),
queryFn: () => adminFetch<SettingsDto>("/api/settings"),
staleTime: 60_000,

// mutation
mutationFn: (body: { socialEnabled: boolean }) =>
  adminFetch<SettingsDto>("/api/settings", {
    method: "PUT",
    body: JSON.stringify(body),
  }),
onSuccess: async (data, _v, _c) => {
  // setQueryData or:
  await queryClient.invalidateQueries({ queryKey: qk.settings.all });
},
```

- [ ] **Step 2: Panel owns form state** from query data when loaded; Save calls mutation with values.

- [ ] **Step 3: Gate + commit**

```bash
cd apps/admin && npm run typecheck && npm run build
rtk git add apps/admin/src/features/settings apps/admin/src/routes/_auth/settings.tsx
rtk git commit -m "feat(admin): settings panel query and mutation"
```

---

### Task 7: Tokens panel

**Files:**
- Create: `apps/admin/src/features/tokens/*`
- Modify: `routes/_auth/tokens.tsx`
- Touch: playground playToken setter via small shared helper or import `PLAY_TOKEN_KEY` + optional callback context — **use-in-playground sets token only, no navigate**

**Interfaces:**
- List `GET /api/tokens`; create `POST` `{ name }` → response includes `token` one-shot → **local `useState newToken`**, not Query
- Delete `DELETE /api/tokens/:id` — confirm via `window.confirm` until Task 12 AlertDialog, or land AlertDialog early if ui kit ready
- Invalidate `qk.tokens.all` after create/delete

- [ ] **Step 1: queries + mutations** matching current `createToken` / `deleteToken` behavior (including create response `row.token`)

- [ ] **Step 2: UI** list + create form + newToken reveal + Use in playground:

```ts
function useInPlayground(token: string) {
  localStorage.setItem(PLAY_TOKEN_KEY, token);
  // if playground state is remote, use a tiny play-token store:
  // playTokenStore.set(token) — or custom event; simplest: localStorage only + playground reads on mount/focus
}
```

Playground feature (Task 11) must read `PLAY_TOKEN_KEY` on mount and when storage updates.

- [ ] **Step 3: Gate + commit**

```bash
rtk git commit -m "feat(admin): tokens panel with query mutations"
```

---

### Task 8: Keys panel

**Files:** `features/keys/*`, `routes/_auth/keys.tsx`

**Interfaces:**
- `GET /api/keys`; `POST /api/keys` `{ service, key }`; `POST /api/keys/:id/toggle`; `DELETE /api/keys/:id`; `POST /api/keys/sync-credits` optional `{ service }`
- Invalidate `qk.keys.all` (and `qk.stats.all` if counts depend on keys)
- **syncCredits honesty:** parse `synced`, `errors`, `results`; partial fail message format must match current strings in `useAdminData.js` (synced/errors/failed ids/ok ids / exa soft-fail note)

- [ ] **Step 1: Port mutations with invalidation** — no full refresh

- [ ] **Step 2: Client-side filter input** over list (SPA UX) — filter local array only

- [ ] **Step 3: Gate + commit**

```bash
rtk git commit -m "feat(admin): keys panel credits sync honesty"
```

---

### Task 9: Nodes panel

**Files:** `features/nodes/*`, `routes/_auth/nodes.tsx`

**Interfaces:**
- `GET /api/nodes`; create `POST` body `{ host, port, username?, password? }` (omit empty username like today); toggle; delete
- Surface `lastError` / consecutive fails fields if present on DTO (from current NodesPanel)
- Invalidate `qk.nodes.all` only

- [ ] **Step 1–3: queries, mutations, panel UI, client filter, gate, commit**

```bash
rtk git commit -m "feat(admin): nodes panel with query mutations"
```

---

### Task 10: Logs panel

**Files:** `features/logs/*`, `routes/_auth/logs.tsx`

**Interfaces:**
- `GET /api/request-logs?limit=50`
- `staleTime: 0`
- Refresh button → `queryClient.invalidateQueries({ queryKey: qk.requestLogs.all })` or `refetch()`
- Client filter over rows; show method field (current honesty)

- [ ] **Step 1–3: implement, gate, commit**

```bash
rtk git commit -m "feat(admin): request logs panel query"
```

---

### Task 11: Playground panel

**Files:** `features/playground/*`, `routes/_auth/playground.tsx`

**Interfaces:**
- **Not** admin Query lists — `runPlayground` uses raw `fetch` + **playToken** Bearer
- Modes: search → `POST /api/search`; extract → `POST /api/extract`; research → `POST /api/research` — bodies exactly as current `runPlayground`
- State: `playToken` (init from `PLAY_TOKEN_KEY`), `playResult`, `playStatus`, `playErr` — React state
- On success: persist playToken to `PLAY_TOKEN_KEY`
- On HTTP error: `playgroundHttpError` helper ported; always set `playStatus` to `res.status`; chip warn on err
- Logout must **not** clear playToken

- [ ] **Step 1: Port `playgroundHttpError` to `features/playground/errors.ts`**

- [ ] **Step 2: `PlaygroundPanel` + `runPlayground` function**

- [ ] **Step 3: Gate + commit**

```bash
rtk git commit -m "feat(admin): API playground panel on playToken"
```

---

### Task 12: Base UI chrome — shell, Toast, AlertDialog, CmdK, sidebar

**Files:**
- Create: `apps/admin/src/components/ui/dialog.tsx`, `alert-dialog.tsx`, `toast.tsx`, `menu.tsx` (thin Cobalt wrappers)
- Create: `apps/admin/src/features/shell/Shell.tsx`, `Topbar.tsx`, `Sidebar.tsx`, `Cmdk.tsx`
- Modify: `routes/_auth.tsx` to render `<Shell><Outlet /></Shell>`
- Modify: destructive deletes in tokens/keys/nodes to AlertDialog
- Modify: `styles.css` / split for shell sidebar layout
- Install: `@base-ui/react`

**Interfaces:**
- CmdK: Dialog + Autocomplete; items from `SECTIONS`; on select `navigate({ to: \`/${id}\` })`
- Topbar Refresh: invalidate **active** panel query key only (derive from router pathname)
- Toast.Provider at root (main or __root); mutation meta messages
- Logout: `auth.logout()` → `queryClient.clear()` → `router.invalidate()` → `navigate({ to: '/login' })`

- [ ] **Step 1: `npm install @base-ui/react`**

- [ ] **Step 2: ui wrappers** — Dialog/AlertDialog/Toast with Cobalt classNames; root `isolation: isolate`

- [ ] **Step 3: Shell layout** sidebar Links + Topbar + Outlet + colophon

- [ ] **Step 4: CmdK** ⌘/Ctrl+K

- [ ] **Step 5: Replace `window.confirm` deletes with AlertDialog

- [ ] **Step 6: Wire toasts** into MutationCache `onSuccess`/`onError` (skip 401)

- [ ] **Step 7: Gate**

```bash
cd apps/admin && npm run typecheck && npm run build
```

Manual: sidebar nav, CmdK jump, delete confirm, toast on save, logout clears lists but keeps playToken.

- [ ] **Step 8: Commit**

```bash
rtk git commit -m "feat(admin): Base UI shell sidebar toast and cmdk"
```

---

### Task 13: Cleanup JS, allowJs off, docs, final gates

**Files:**
- Delete obsolete: `App.jsx`, `api.js`, `constants.js`, `hooks/*`, old `components/**/*.jsx` once unused
- Modify: `tsconfig.app.json` → `"allowJs": false`
- Modify: `docs/ops/deploy.md`, `docs/ops/env.md`, `README.md`, `AGENTS.md` — replace `vite` / bare build narrative with `npm run dev|build` (Vite+) and Node 22.18+ note
- Verify: `Dockerfile` + `ci.yml` still green path

**Interfaces:** none new — end-state success criteria from spec

- [ ] **Step 1: rg for stale imports**

```bash
rg -n "useAdminData|useAdminSession|from monologue|App\.jsx|constants\.js|api\.js" apps/admin/src
```

Expected: no hits (or only comments none).

- [ ] **Step 2: Delete dead JS; set `allowJs: false`**

- [ ] **Step 3: Full gate**

```bash
cd apps/admin && npm run typecheck && npm run check && npm run build
rg -n '/admin/' apps/admin/dist/index.html
# optional local image:
# docker build -t serpotter:admin-spa .
```

Expected: zero JS under `src/`; typecheck clean; dist `/admin/` assets.

- [ ] **Step 4: Manual smoke checklist**

1. `/admin/login` — secret, password, bootstrap modes (as env allows)
2. Each panel loads
3. One create + toggle + delete (keys or nodes)
4. CmdK → keys
5. Use-in-playground sets token; open playground; no auto-nav from tokens
6. Logout → login; playToken still in localStorage
7. Bad/expired session → 401 → login

- [ ] **Step 5: Docs commit**

```bash
rtk git add docs/ops/deploy.md docs/ops/env.md README.md AGENTS.md apps/admin
rtk git commit -m "docs(admin): Vite+ SPA build and cutover notes"
```

- [ ] **Step 6: Final commit if cleanup separate**

```bash
rtk git commit -m "refactor(admin): remove legacy JSX admin shell"
```

---

## Plan self-review

| Spec requirement | Task |
| --- | --- |
| Vite+ first + shared npm build local/CI/Docker | 1 |
| Node engines 22.18+ pin | 1 |
| Strict TS scaffold + later allowJs false | 2, 13 |
| lib api/keys/auth/safeRedirect | 3 |
| Router basepath, login, guards path-only redirect | 4 |
| Per-resource Query panels | 5–11 |
| playToken survives; playground honesty | 11, 12 logout |
| Base UI shell/toast/cmdk/AlertDialog | 12 |
| Docs + zero JS | 13 |
| No new backend / no Zod / no optimistic v1 | Global Constraints |

**Placeholder scan:** none intentional. Panel tasks 6–11 are thinner than Task 5 by design — implementers copy Task 5 query pattern and existing `useAdminData.js` mutation bodies verbatim.

**Type consistency:** `safeRedirectPath`, `qk.*`, `adminFetch<T>`, `AuthContextValue`, `createAppQueryClient`, `basepath: '/admin'`, scripts `vp build` (T1) then `tsc -b && vp build` (T2+).

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-29-admin-spa-tanstack-viteplus.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks (`subagent-driven-development`)
2. **Parallel Independent Domains** — only after Task 4; panels 5–11 can parallelize if agents own disjoint `features/<name>/` paths (`dispatching-parallel-agents`)

**Which approach?**

