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

Create `features/auth/session-end.ts` (used by Query 401 and logout — avoids router↔auth import cycles):

```ts
import { SECRET_KEY, SESSION_EXPIRES_KEY, SESSION_KEY } from "@/lib/constants";

/** Clears admin identity only — never PLAY_TOKEN_KEY. */
export function clearAuthStorage(): void {
  localStorage.removeItem(SECRET_KEY);
  localStorage.removeItem(SESSION_KEY);
  localStorage.removeItem(SESSION_EXPIRES_KEY);
}
```

`AuthProvider` core (mirror `useAdminSession.js` line-for-line semantics):

```tsx
const [token, setToken] = useState(
  () => localStorage.getItem(SESSION_KEY) || localStorage.getItem(SECRET_KEY) || "",
);
const [sessionExpiresAt, setSessionExpiresAt] = useState(
  () => localStorage.getItem(SESSION_EXPIRES_KEY) || "",
);

const applySecretToken = (s: string) => {
  localStorage.setItem(SECRET_KEY, s);
  localStorage.removeItem(SESSION_KEY);
  localStorage.removeItem(SESSION_EXPIRES_KEY);
  setToken(s);
  setSessionExpiresAt("");
  setErr("");
};

const applySessionToken = (t: string, expiresAt?: string) => {
  localStorage.setItem(SESSION_KEY, t);
  localStorage.removeItem(SECRET_KEY);
  if (expiresAt) {
    localStorage.setItem(SESSION_EXPIRES_KEY, String(expiresAt));
    setSessionExpiresAt(String(expiresAt));
  } else {
    localStorage.removeItem(SESSION_EXPIRES_KEY);
    setSessionExpiresAt("");
  }
  setToken(t);
  setErr("");
};

const clearAuth = () => {
  clearAuthStorage();
  setToken("");
  setSessionExpiresAt("");
  setErr("");
  setBusy(false);
};

const logout = () => {
  const session = localStorage.getItem(SESSION_KEY);
  if (session) {
    void fetch(`${apiBase()}/api/admin/logout`, {
      method: "POST",
      headers: { Authorization: `Bearer ${session}` },
    }).catch(() => {});
  }
  clearAuth();
};

// loginWithPasswordHttp: POST /api/admin/login JSON { username, password }
//   → parseJsonResponse → { token, expiresAt: data.expiresAt || data.expires_at || "" }
// bootstrapHttp: POST /api/admin/bootstrap Bearer adminSecret, body { password, username? if trim non-empty }
//   then same login as above with username default "admin"
```

Export `useAuth()` that throws if outside provider.

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

- [ ] **Step 5: `main.tsx` provider tree + session end (one recipe only)**

**401 path — `endAdminSession` (Query cache onError):**

```ts
// src/lib/session-end-app.ts
import { clearAuthStorage } from "@/features/auth/session-end";
import { router } from "@/router";
import type { QueryClient } from "@tanstack/react-query";

export function endAdminSession(queryClient: QueryClient): void {
  clearAuthStorage();
  queryClient.clear();
  window.dispatchEvent(new Event("serpotter:auth-cleared"));
  void router.navigate({ to: "/login" });
  void router.invalidate();
}
```

**AuthProvider** must subscribe (same task as AuthProvider, Task 3 — if not already, add here):

```ts
useEffect(() => {
  const fn = () => {
    setToken("");
    setSessionExpiresAt("");
    setErr("");
    setBusy(false);
  };
  window.addEventListener("serpotter:auth-cleared", fn);
  return () => window.removeEventListener("serpotter:auth-cleared", fn);
}, []);
```

**main.tsx:**

```tsx
const queryClient = createAppQueryClient({
  onUnauthorized: () => endAdminSession(queryClient),
});

function InnerApp() {
  const auth = useAuth();
  return (
    <RouterProvider router={router} context={{ auth, queryClient }} />
  );
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <AuthProvider>
        <InnerApp />
      </AuthProvider>
    </QueryClientProvider>
  </StrictMode>,
);
```

**Explicit logout (Shell / Task 12) — do not call `endAdminSession` (would double-clear):**

```ts
function onLogout() {
  auth.logout(); // best-effort POST /api/admin/logout + clearAuth (storage + React)
  queryClient.clear();
  void router.navigate({ to: "/login" });
  void router.invalidate();
}
```

`auth.logout()` never touches `PLAY_TOKEN_KEY`. `queryClient.clear()` drops admin list caches only.

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
- Create: `apps/admin/src/features/stats/types.ts`
- Create: `apps/admin/src/features/stats/queries.ts`
- Create: `apps/admin/src/features/stats/StatsPanel.tsx`
- Modify: `apps/admin/src/routes/_auth/stats.tsx` → render `StatsPanel`
- Read first: `apps/admin/src/components/panels/StatsPanel.jsx` for fields actually rendered (include `schemaVersion`)

**Interfaces:**
- Consumes: `adminFetch`, `qk.stats`, `useQuery` / `queryOptions`
- Produces: pattern later panels copy

```ts
// queries.ts
import { queryOptions } from "@tanstack/react-query";
import { adminFetch } from "@/lib/api";
import { qk } from "@/lib/query-keys";
import type { StatsDto } from "./types";

export const statsQueryOptions = queryOptions({
  queryKey: qk.stats.summary(),
  queryFn: () => adminFetch<StatsDto>("/api/stats"),
  staleTime: 10_000,
});
```

- [ ] **Step 1: Define `StatsDto`** only from fields StatsPanel/Topbar use today (no invented API fields).

- [ ] **Step 2: Implement `StatsPanel` with `useQuery(statsQueryOptions)`**

States: pending spinner; error region + `refetch` button; success metric strip / definition list (Cobalt — not SaaS card grid).

- [ ] **Step 3: Wire route**

```tsx
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

Manual: login → `/admin/stats` loads or shows error without crashing shell.

- [ ] **Step 5: Commit**

```bash
rtk git add apps/admin/src/features/stats apps/admin/src/routes/_auth/stats.tsx
rtk git commit -m "feat(admin): stats panel with TanStack Query"
```

---

### Task 6: Settings panel

**Files:**
- Create: `apps/admin/src/features/settings/types.ts`
- Create: `apps/admin/src/features/settings/queries.ts`
- Create: `apps/admin/src/features/settings/SettingsPanel.tsx`
- Modify: `apps/admin/src/routes/_auth/settings.tsx`
- Read first: `components/panels/SettingsPanel.jsx` + `saveSettings` in `useAdminData.js`

**Interfaces:**

```ts
export const settingsQueryOptions = queryOptions({
  queryKey: qk.settings.root(),
  queryFn: () => adminFetch<SettingsDto>("/api/settings"),
  staleTime: 60_000,
});

// useMutation
mutationFn: (body: { socialEnabled: boolean }) =>
  adminFetch<SettingsDto>("/api/settings", {
    method: "PUT",
    body: JSON.stringify({ socialEnabled: body.socialEnabled }),
  }),
onSuccess: async (data) => {
  qc.setQueryData(qk.settings.root(), data);
  // or: await qc.invalidateQueries({ queryKey: qk.settings.all });
},
```

- [ ] **Step 1: queries + save mutation** as above (`socialEnabled` only — match current PUT body)

- [ ] **Step 2: Panel owns form state** hydrated from query `data` when settled; Save calls `mutate({ socialEnabled })`; disable while `isPending`

- [ ] **Step 3: Gate + commit**

```bash
cd apps/admin && npm run typecheck && npm run build
rtk git add apps/admin/src/features/settings apps/admin/src/routes/_auth/settings.tsx
rtk git commit -m "feat(admin): settings panel query and mutation"
```

---

### Task 7: Tokens panel

**Files:**
- Create: `apps/admin/src/features/tokens/types.ts`
- Create: `apps/admin/src/features/tokens/queries.ts`
- Create: `apps/admin/src/features/tokens/TokensPanel.tsx`
- Modify: `apps/admin/src/routes/_auth/tokens.tsx`
- Read first: `components/panels/TokensPanel.jsx` + `createToken`/`deleteToken`/`useInPlayground` in `useAdminData.js`

**Interfaces:**

```ts
export const tokensQueryOptions = queryOptions({
  queryKey: qk.tokens.list(),
  queryFn: () => adminFetch<TokenRow[]>("/api/tokens"),
});

// createToken — response includes one-shot plaintext token
mutationFn: async (p: { name: string }) => {
  const row = await adminFetch<{ token?: string }>("/api/tokens", {
    method: "POST",
    body: JSON.stringify({ name: p.name }),
  });
  return row;
},
onSuccess: async (row) => {
  // caller sets local newToken state from row.token || ""
  await qc.invalidateQueries({ queryKey: qk.tokens.all });
},

// deleteToken
mutationFn: (id: string | number) =>
  adminFetch(`/api/tokens/${id}`, { method: "DELETE" }),
onSuccess: async () => {
  await qc.invalidateQueries({ queryKey: qk.tokens.all });
},
```

**Use in playground (no navigate):**

```ts
import { PLAY_TOKEN_KEY } from "@/lib/constants";

export function useInPlayground(token: string): void {
  localStorage.setItem(PLAY_TOKEN_KEY, token);
  window.dispatchEvent(new Event("serpotter:play-token"));
}
```

Playground (Task 11) initializes from `PLAY_TOKEN_KEY` and listens for `serpotter:play-token`.

- [ ] **Step 1: queries + create/delete mutations**

- [ ] **Step 2: Panel** — list, create name form, **local `useState` `newToken`** from create response (not Query), delete confirm (`window.confirm` until Task 12), Use in playground button, optional client filter

- [ ] **Step 3: Gate + commit**

```bash
cd apps/admin && npm run typecheck && npm run build
rtk git add apps/admin/src/features/tokens apps/admin/src/routes/_auth/tokens.tsx
rtk git commit -m "feat(admin): tokens panel with query mutations"
```

---

### Task 8: Keys panel

**Files:**
- Create: `apps/admin/src/features/keys/types.ts`
- Create: `apps/admin/src/features/keys/queries.ts`
- Create: `apps/admin/src/features/keys/KeysPanel.tsx`
- Modify: `apps/admin/src/routes/_auth/keys.tsx`
- Read first: `components/panels/KeysPanel.jsx` + keys section of `useAdminData.js`

**Interfaces:**

```ts
export const keysQueryOptions = queryOptions({
  queryKey: qk.keys.list(),
  queryFn: () => adminFetch<KeyRow[]>("/api/keys"),
});

// createKey
mutationFn: (p: { service: string; key: string }) =>
  adminFetch("/api/keys", {
    method: "POST",
    body: JSON.stringify({ service: p.service, key: p.key }),
  }),
onSuccess: async () => {
  await Promise.all([
    qc.invalidateQueries({ queryKey: qk.keys.all }),
    qc.invalidateQueries({ queryKey: qk.stats.all }),
  ]);
},

// toggleKey
mutationFn: (id: string | number) =>
  adminFetch(`/api/keys/${id}/toggle`, { method: "POST" }),
onSuccess: async () => {
  await qc.invalidateQueries({ queryKey: qk.keys.all });
},

// deleteKey
mutationFn: (id: string | number) =>
  adminFetch(`/api/keys/${id}`, { method: "DELETE" }),
onSuccess: async () => {
  await Promise.all([
    qc.invalidateQueries({ queryKey: qk.keys.all }),
    qc.invalidateQueries({ queryKey: qk.stats.all }),
  ]);
},
```

- [ ] **Step 1: List/create/toggle/delete** as above

- [ ] **Step 2: `syncCredits` honesty — strings must match `useAdminData.js` exactly**

```ts
type SyncReport = {
  synced?: number;
  errors?: number;
  results?: Array<{ id?: string | number; ok?: boolean; error?: string }>;
};

mutationFn: async (p?: { service?: string }) => {
  const body: Record<string, string> = {};
  if (p?.service) body.service = p.service;
  return adminFetch<SyncReport>("/api/keys/sync-credits", {
    method: "POST",
    body: JSON.stringify(body),
  });
},
onSuccess: async (report) => {
  await qc.invalidateQueries({ queryKey: qk.keys.all });
  const synced = Number(report?.synced ?? 0);
  const errors = Number(report?.errors ?? 0);
  const results = Array.isArray(report?.results) ? report.results : [];
  const failed = results.filter((r) => r && r.ok === false);
  const ok = results.filter((r) => r && r.ok === true);
  const failDetail =
    failed.length > 0
      ? `; failed: ${failed
          .map((r) => (r.error ? `#${r.id}: ${r.error}` : `#${r.id}`))
          .join(",")}`
      : "";
  const okDetail =
    ok.length > 0 && errors > 0
      ? `; ok: ${ok.map((r) => `#${r.id}`).join(",")}`
      : "";
  if (errors > 0) {
    // error toast / setError with EXACT string:
    const msg = `Credit sync partial: synced=${synced}, errors=${errors}${failDetail}${okDetail} (exa/xai soft-fail or fetch error; keys stay active)`;
    throw new Error(msg);
  }
  // success toast EXACT: `Credit sync: synced=${synced}, errors=0`
  return `Credit sync: synced=${synced}, errors=0`;
},
```

Partial → error feedback; clean → success toast; never silent.

- [ ] **Step 3: Panel UI** — list, create (service + key), toggle, delete, sync button, client `useMemo` filter

- [ ] **Step 4: Gate + commit**

```bash
cd apps/admin && npm run typecheck && npm run build
rtk git add apps/admin/src/features/keys apps/admin/src/routes/_auth/keys.tsx
rtk git commit -m "feat(admin): keys panel credits sync honesty"
```

---

### Task 9: Nodes panel

**Files:**
- Create: `apps/admin/src/features/nodes/types.ts`
- Create: `apps/admin/src/features/nodes/queries.ts`
- Create: `apps/admin/src/features/nodes/NodesPanel.tsx`
- Modify: `apps/admin/src/routes/_auth/nodes.tsx`
- Read first: `components/panels/NodesPanel.jsx` for `lastError` / consecutive fails / host fields

**Interfaces:**

```ts
export const nodesQueryOptions = queryOptions({
  queryKey: qk.nodes.list(),
  queryFn: () => adminFetch<NodeRow[]>("/api/nodes"),
});

// createNode — omit empty username like useAdminData
mutationFn: async (p: {
  host: string;
  port: number | string;
  username?: string;
  password?: string;
}) => {
  const body: Record<string, unknown> = {
    host: String(p.host ?? "").trim(),
    port: Number(p.port),
  };
  const user = p.username != null ? String(p.username).trim() : "";
  if (user) body.username = user;
  if (p.password) body.password = p.password;
  return adminFetch("/api/nodes", {
    method: "POST",
    body: JSON.stringify(body),
  });
},
onSuccess: async () => {
  await qc.invalidateQueries({ queryKey: qk.nodes.all });
},

mutationFn: (id: string | number) =>
  adminFetch(`/api/nodes/${id}/toggle`, { method: "POST" }),
// onSuccess → invalidate qk.nodes.all

mutationFn: (id: string | number) =>
  adminFetch(`/api/nodes/${id}`, { method: "DELETE" }),
// onSuccess → invalidate qk.nodes.all
```

- [ ] **Step 1: queries + create/toggle/delete mutations**

- [ ] **Step 2: Panel** — form host/port/username/password; list with lastError/fails; toggle; delete; client filter

- [ ] **Step 3: Gate + commit**

```bash
cd apps/admin && npm run typecheck && npm run build
rtk git add apps/admin/src/features/nodes apps/admin/src/routes/_auth/nodes.tsx
rtk git commit -m "feat(admin): nodes panel with query mutations"
```

---

### Task 10: Logs panel

**Files:**
- Create: `apps/admin/src/features/logs/types.ts`
- Create: `apps/admin/src/features/logs/queries.ts`
- Create: `apps/admin/src/features/logs/LogsPanel.tsx`
- Modify: `apps/admin/src/routes/_auth/logs.tsx`
- Read first: `components/panels/LogsPanel.jsx` (must show **method**)

**Interfaces:**

```ts
export const requestLogsQueryOptions = queryOptions({
  queryKey: qk.requestLogs.list({ limit: 50 }),
  queryFn: async () => {
    const logs = await adminFetch<RequestLogRow[]>(
      "/api/request-logs?limit=50",
    );
    return Array.isArray(logs) ? logs : [];
  },
  staleTime: 0,
});
```

- [ ] **Step 1: Query + table** including method column

- [ ] **Step 2: Refresh** = `refetch()` from `useQuery` only (old `refreshLogsOnly`)

- [ ] **Step 3: Client filter** on path/method/status substring

- [ ] **Step 4: Gate + commit**

```bash
cd apps/admin && npm run typecheck && npm run build
rtk git add apps/admin/src/features/logs apps/admin/src/routes/_auth/logs.tsx
rtk git commit -m "feat(admin): request logs panel query"
```

---

### Task 11: Playground panel

**Files:**
- Create: `apps/admin/src/features/playground/errors.ts`
- Create: `apps/admin/src/features/playground/runPlayground.ts`
- Create: `apps/admin/src/features/playground/PlaygroundPanel.tsx`
- Modify: `apps/admin/src/routes/_auth/playground.tsx`

**Interfaces:** Not admin list Query. `playToken` ↔ `PLAY_TOKEN_KEY`. Results are React state.

- [ ] **Step 1: Error helper (exact port)**

```ts
// features/playground/errors.ts
export function playgroundHttpError(
  res: Response,
  data: unknown,
  text: string,
): string {
  if (typeof data === "object" && data !== null) {
    const rec = data as { title?: unknown; detail?: unknown };
    const title = rec.title != null ? String(rec.title).trim() : "";
    const detail = rec.detail != null ? String(rec.detail).trim() : "";
    if (title && detail) return `${res.status} ${title}: ${detail}`;
    if (title) return `${res.status} ${title}`;
    if (detail) return `${res.status} ${detail}`;
  }
  const fallback =
    (typeof data === "string" && data) ||
    text ||
    res.statusText ||
    "request failed";
  return `${res.status} ${fallback}`;
}
```

- [ ] **Step 2: `runPlayground` — full logic from `useAdminData.js`**

```ts
import { apiBase } from "@/lib/api";
import { PLAY_TOKEN_KEY } from "@/lib/constants";
import { playgroundHttpError } from "./errors";

export type RunPlaygroundArgs = {
  token: string;
  mode?: string;
  query?: string;
  maxResults?: number | string;
  url?: string;
  scrapeTopN?: number | string;
};

export type RunPlaygroundResult =
  | { ok: true; status: number; data: unknown }
  | { ok: false; status: number | null; error: string };

export async function runPlayground(
  args: RunPlaygroundArgs,
): Promise<RunPlaygroundResult> {
  const m = String(args.mode ?? "search").trim().toLowerCase() || "search";
  let path: string;
  let body: Record<string, unknown>;
  if (m === "extract") {
    path = "/api/extract";
    body = { url: String(args.url ?? "").trim() };
  } else if (m === "research") {
    path = "/api/research";
    body = { query: String(args.query ?? "").trim() };
    const maxN = Number(args.maxResults);
    if (Number.isFinite(maxN) && maxN > 0) body.maxResults = maxN;
    const scrapeN = Number(args.scrapeTopN);
    if (
      Number.isFinite(scrapeN) &&
      scrapeN >= 0 &&
      String(args.scrapeTopN ?? "").trim() !== ""
    ) {
      body.scrapeTopN = scrapeN;
    }
  } else {
    path = "/api/search";
    body = {
      query: String(args.query ?? "").trim(),
      maxResults: Number(args.maxResults) || 5,
    };
  }

  try {
    const res = await fetch(`${apiBase()}${path}`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${String(args.token ?? "").trim()}`,
        "content-type": "application/json",
      },
      body: JSON.stringify(body),
    });
    const text = await res.text();
    let data: unknown;
    try {
      data = text ? JSON.parse(text) : null;
    } catch {
      data = text;
    }
    if (!res.ok) {
      return {
        ok: false,
        status: res.status,
        error: playgroundHttpError(res, data, text),
      };
    }
    localStorage.setItem(PLAY_TOKEN_KEY, String(args.token ?? "").trim());
    return { ok: true, status: res.status, data };
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    return { ok: false, status: null, error: msg };
  }
}
```

- [ ] **Step 3: Panel state + UI**

```ts
const [playToken, setPlayToken] = useState(
  () => localStorage.getItem(PLAY_TOKEN_KEY) || "",
);
useEffect(() => {
  const sync = () =>
    setPlayToken(localStorage.getItem(PLAY_TOKEN_KEY) || "");
  window.addEventListener("serpotter:play-token", sync);
  return () => window.removeEventListener("serpotter:play-token", sync);
}, []);

const [playResult, setPlayResult] = useState<unknown>(null);
const [playStatus, setPlayStatus] = useState<number | null>(null);
const [playErr, setPlayErr] = useState("");
const [pending, setPending] = useState(false);

// on submit:
setPlayErr("");
setPlayResult(null);
setPlayStatus(null);
setPending(true);
const out = await runPlayground({
  token: playToken,
  mode,
  query,
  maxResults,
  url,
  scrapeTopN,
});
setPending(false);
if (out.ok) {
  setPlayStatus(out.status);
  setPlayResult(out.data);
  setPlayErr("");
} else {
  setPlayStatus(out.status);
  setPlayErr(out.error);
  setPlayResult(null);
}
```

Modes: search | extract | research. On error show status with existing **`chip--warn`**. Logout must not clear `PLAY_TOKEN_KEY`.

- [ ] **Step 4: Gate + commit**

```bash
cd apps/admin && npm run typecheck && npm run build
rtk git add apps/admin/src/features/playground apps/admin/src/routes/_auth/playground.tsx
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

**Placeholder scan:** Tasks 5–11 include concrete query/mutation bodies (including syncCredits honesty strings and full `runPlayground` / `playgroundHttpError`). Auth session-end uses `clearAuthStorage` + `endAdminSession` + `serpotter:auth-cleared`.

**Type consistency:** `safeRedirectPath`, `qk.*`, `adminFetch<T>`, `AuthContextValue`, `createAppQueryClient`, `basepath: '/admin'`, scripts `vp build` (T1) then `tsc -b && vp build` (T2+).

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-29-admin-spa-tanstack-viteplus.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks (`subagent-driven-development`)
2. **Parallel Independent Domains** — only after Task 4; panels 5–11 can parallelize if agents own disjoint `features/<name>/` paths (`dispatching-parallel-agents`)

**Which approach?**

