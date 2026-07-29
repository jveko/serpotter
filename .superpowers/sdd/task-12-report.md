# Task 12 report — Base UI chrome (shell, Toast, AlertDialog, CmdK, sidebar)

**Status:** DONE  
**Commit:** `ccb9504` (`ccb95042ea2314025b9b4a15fd80e1608591b365`)
**Worktree:** `feat-admin-spa-tanstack-viteplus` @ BASE `cff0e3c`

## What landed

### Install
- `@base-ui/react@^1.6.0` in `apps/admin/package.json` (+ lockfile)

### UI wrappers (`apps/admin/src/components/ui/`)
- `dialog.tsx` — Cobalt-classed Base UI Dialog parts
- `alert-dialog.tsx` — AlertDialog parts + `ConfirmDeleteDialog`
- `toast.tsx` — `createToastManager` singleton, Provider/List/`showToast`
- `menu.tsx` — thin Menu wrapper (optional overflow polish)

### Shell (`apps/admin/src/features/shell/`)
- `Shell.tsx` — Topbar + Sidebar + main Outlet area + colophon + CmdK; global ⌘/Ctrl+K
- `Topbar.tsx` — schema chip (stats query), session exp, Jump, Refresh (active panel query key only from pathname), Logout recipe
- `Sidebar.tsx` — `Link` per `SECTIONS` with router active state
- `Cmdk.tsx` — Dialog + Autocomplete; jump via `Autocomplete.Item` `onClick` (pointer + Enter)

### Wiring
- `_auth.tsx` → `<Shell><Outlet /></Shell>`
- `main.tsx` → `.root` + `Toast.Provider` + portal viewport/list around app
- `query-client.ts` → MutationCache success/error toasts via `meta.successMessage` / `errorMessage`; skip 401 (still `endAdminSession`); `meta.silent` for keys sync honesty

### Panels
- tokens / keys / nodes: `window.confirm` → `ConfirmDeleteDialog`
- mutation `meta.successMessage` on create/delete/toggle/save (settings); keys sync `silent: true`

### Styles
- `.root { isolation: isolate }`
- `.shell__body` row (sidebar + main), sticky sidebar ≥48rem, mobile horizontal section strip
- Base UI dialog/alert/toast/menu + cmdk viewport/highlight styles

## Logout recipe (explicit)
`auth.logout()` → `queryClient.clear()` → `navigate('/login')` → `router.invalidate()`  
Does **not** call `endAdminSession`. Does **not** clear `PLAY_TOKEN_KEY`.

## Gates
```text
cd apps/admin && npm run typecheck   # exit 0
cd apps/admin && npm run build       # exit 0
```

## Concerns / follow-ups
- Manual browser smoke (sidebar, CmdK, delete dialog, toast, logout playToken) deferred to Task 13 checklist
- Narrow viewport sidebar is a horizontal strip, not a full drawer Dialog (design allowed drawer; strip is lighter)
- ~~Autocomplete selection depends on Base UI `fillInputOnItemPress` + `reason === "item-press"` mapping label→section~~ **Fixed:** see Critical fix below
- Keys sync still uses local honesty notice (`silent`) rather than global toast

## Critical fix — CmdK jump (item-press never fired)

**Finding:** `inline open` Autocomplete without `Popup` leaves popupRef null, so `onValueChange` never sees `details.reason === "item-press"` and navigate never ran on click/Enter.

**Fix:** `Autocomplete.Item onClick={() => jump(item.id)}` — Base UI docs: fires on pointer click and Enter when the item is highlighted. Dropped `resolveSection` + item-press branch; `onValueChange` only updates filter query.

**File:** `apps/admin/src/features/shell/Cmdk.tsx`

**Gates:** `npm run typecheck` + `npm run build` (apps/admin) exit 0.

**Tests:** no automated CmdK interaction tests; contract verified by typecheck/build + Base UI `AutocompleteItem` onClick docs (pointer + Enter). Manual smoke still Task 13.
