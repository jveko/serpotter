## Task 8 report
- **Status:** done
- **Commit:** `5f3af50` feat(admin): keys panel credits sync honesty
- **Tests:** `npm run typecheck` + `npm run build` green in `apps/admin`
- **Delivered:**
  - `apps/admin/src/features/keys/types.ts` — `KeyRow`, `SyncKeyResult`, `SyncReport`
  - `apps/admin/src/features/keys/queries.ts` — `keysQueryOptions` + create/toggle/delete/syncCredits requests
  - `apps/admin/src/features/keys/KeysPanel.tsx` — list, seed, filter, toggle, delete, sync UI
  - `apps/admin/src/routes/_auth/keys.tsx` — wires `KeysPanel` (stub replaced)
- **Behavior:**
  - List via `keysQueryOptions` (`qk.keys.list()`)
  - create invalidates `qk.keys.all` + `qk.stats.all`; clears key input on success
  - toggle invalidates `qk.keys.all`
  - delete confirms + invalidates keys + stats
  - `syncCreditsRequest`: honesty in **mutationFn** (RQ v5-safe)
    - partial (`errors>0`): throw exact `Credit sync partial: synced=…, errors=…${failDetail}${okDetail} (exa/xai soft-fail or fetch error; keys stay active)`
    - clean: return exact `Credit sync: synced=${synced}, errors=0`
  - failDetail/okDetail match `useAdminData.js`
  - sync `onSettled` always invalidates keys (partial still mutates server rows)
  - partial → error banner; clean → status notice; never silent
- **Concerns:** none
- **Path:** `.worktrees/feat-admin-spa-tanstack-viteplus`

## Fix: sticky syncMutation.error (review Important)
- **Problem:** After partial credit sync, `syncMutation.error` stuck; `mutErr` ORs all mutation errors so successful create/toggle/delete still showed `Credit sync partial…`.
- **Fix:** `syncMutation.reset()` at start of create/toggle/delete (handlers); wire toggle via `handleToggle`. On sync start, reset create/toggle/delete + clear `syncNotice` so other sticky errors don't mask a new sync.
- **Verify:** `cd apps/admin && npm run typecheck && npm run build` — green.
