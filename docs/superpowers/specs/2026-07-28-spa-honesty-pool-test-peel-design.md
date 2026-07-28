# SPA Honesty + Pool Test Peel Design

**Date:** 2026-07-28  
**Status:** Approved for implementation planning  
**Scope:** Admin SPA operator-feedback honesty (A1+A2) and optional keypool/outbound unit-test module peels (C)

## Problem

Post residual polish and fat-file restructure:

1. **Admin SPA** — most honesty surfaces (session, KeyOut columns, nodes toggle, stats, logs) are landed. Two thin operator-friction gaps remain:
   - Credit sync is **silent on full success** (`errors === 0`): lists refresh with no banner.
   - Playground **drops HTTP status on non-2xx**: chip only appears on success; err text often loses status.
2. **keypool / outbound** — production ~200 LOC each; large inline `#[cfg(test)]` blocks inflate `lib.rs` (~472 / ~392 total). Optional navigation hygiene only (not a LOC-cap requirement).

## Goals

1. Always surface credit-sync outcome (success and partial) after `POST /api/keys/sync-credits`.
2. Playground non-2xx keeps `playStatus` and problem-shaped error text (status + title/detail when present).
3. Peel keypool and outbound unit tests into `src/tests.rs` without API or behavior change.

## Non-goals

- Advanced playground knobs (strategy, provider, dates, domains, handles, mode)
- New admin APIs or request-log server-side filters
- Shared search/extract attempt-matrix helper
- `error.rs` peels or production splits in keypool/outbound
- Wire/schema/crate-graph changes
- Re-implementing residual A–F / twin-pool / MCP residual polish items

## Decisions (locked)

| Decision | Choice |
| --- | --- |
| SPA package | **A1 + A2 only** |
| A1 channel | Success → non-error notice; partial (`errors > 0`) → red err (existing semantics); throw → `reportError` |
| A2 chip | Show status chip whenever `playStatus != null`, including error-only path; not `chip--ok` on failure |
| C peels | **Both** keypool and outbound |
| C layout | `lib.rs` production + `#[cfg(test)] mod tests;` → body in `src/tests.rs` |
| C error.rs | **No** |
| API changes | **None** |

## A1 — Credit-sync feedback

### Today

`apps/admin/src/hooks/useAdminData.js` `syncCredits`:

- Parses `synced`, `errors`, `results` from report.
- Builds `partialMsg` only when `errors > 0`.
- `await refresh(secret)` then `if (partialMsg) setErr(partialMsg)`.
- Full success: refresh only → silent.

### Target behavior

| Outcome | UI |
| --- | --- |
| `errors === 0` | Non-error notice, e.g. `Credit sync: synced=N, errors=0` (optional ok id list if useful and short) |
| `errors > 0` | Red err: `Credit sync partial: synced=…, errors=…` + fail/ok id detail (keep current honesty about exa/xai soft-fail) |
| HTTP/throw | Unchanged `reportError` |

### Implementation notes

- Prefer a dedicated **success/notice** state if the app already has one; otherwise add minimal `okMsg` / `notice` cleared on next mutation or refresh, **not** reuse red `err` for success.
- Re-apply notice **after** `refresh` (refresh currently clears err — same ordering trap as partial).
- KeysPanel may only need to display the shared banner from App; avoid duplicating fetch logic.
- No change to `POST /api/keys/sync-credits` or `SyncCredits` JSON shape (`synced`, `errors`, `results[]`).

## A2 — Playground error honesty

### Today

`runPlayground`:

- On `!res.ok`, throws `Error` from `detail` / `title` / text — **does not** `setPlayStatus`.
- `setPlayStatus(res.status)` only on success.
- `PlaygroundPanel` shows `chip--ok` only beside `playResult`.

### Target behavior

1. On every completed HTTP response (success or not): **`setPlayStatus(res.status)`**.
2. Error message includes status and problem fields when present, e.g.:
   - object with `title`/`detail`: `` `${res.status} ${title}: ${detail}` `` (trim empty parts)
   - else text or `res.statusText` with status prefix
3. UI: if `playStatus != null` and error path (no success result, or explicit err), render chip with warn/error class (e.g. `chip--warn` or existing danger class), text like `` `${playStatus}` `` or `` `${playStatus} error` `` — **not** `chip--ok`.
4. Keep `playErr` paragraph.

### Non-goals (A2)

- Expanding request body beyond current mode fields
- Changing product REST error mapping

## C — keypool + outbound test peel

### Recipe (both crates)

```text
src/lib.rs
  … production …
  #[cfg(test)]
  mod tests;

src/tests.rs
  // former interior of mod tests { … } only
  // use super::*; etc.
```

| Crate | Production stays | Move |
| --- | --- | --- |
| `serpotter-keypool` | ~lines 1–203 | ~204–472 test module body |
| `serpotter-outbound` | ~lines 1–203 | ~205–392 test module body |

### Rules

- Do **not** nest `mod tests {` inside `tests.rs` (would become `tests::tests`).
- Private helpers stay in `lib.rs`; tests keep `use super::*`.
- No Cargo.toml / pub API changes.
- First monorepo `src/tests.rs` peels — local hygiene, not a mandate for every crate.

## File map

| File | Role |
| --- | --- |
| `apps/admin/src/hooks/useAdminData.js` | A1 sync notice; A2 status + err text |
| `apps/admin/src/components/panels/PlaygroundPanel.jsx` | A2 chip on error path |
| `apps/admin/src/App.jsx` and/or `KeysPanel.jsx` | Wire success notice if new state |
| `crates/serpotter-keypool/src/lib.rs` | Drop inline tests; `mod tests;` |
| `crates/serpotter-keypool/src/tests.rs` | Unit tests body |
| `crates/serpotter-outbound/src/lib.rs` | Same |
| `crates/serpotter-outbound/src/tests.rs` | Unit tests body |

## Testing / gates

```bash
rtk cargo test -p serpotter-keypool -p serpotter-outbound
rtk cargo clippy -p serpotter-keypool -p serpotter-outbound -- -D warnings
cd apps/admin && npm run build
```

Manual SPA (if dev server available):

- Sync credits all → notice with synced count
- Force partial (if feasible) → red partial message
- Playground bad token / missing query → status chip + err with status

No new contract tests required for pure SPA copy/state; pool peel must keep all existing unit tests green.

## Risks

| Risk | Mitigation |
| --- | --- |
| Success notice uses red err | Dedicated notice state |
| refresh clears notice | Set notice after refresh |
| `tests.rs` double-wrap | Body-only under `mod tests;` |
| CSS missing warn chip | Reuse existing chip modifiers or minimal class already in admin CSS |

## Acceptance

1. Full credit sync success shows a non-error report with `synced`/`errors`.
2. Partial sync still uses error styling with counts + fail detail.
3. Playground non-2xx sets `playStatus` and shows non-ok chip + status-bearing err text.
4. keypool + outbound tests pass from `tests.rs`; production lib smaller; public APIs unchanged.
5. `npm run build` for admin succeeds.

## Implementation next step

After written-spec approval: **writing-plans** → `docs/superpowers/plans/2026-07-28-spa-honesty-pool-test-peel.md`, then implement (C mechanical first or parallel with A).
