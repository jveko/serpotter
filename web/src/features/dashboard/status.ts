/**
 * Dashboard status: which card reads which queries, and what the page head
 * prints while they load, fail, or refresh.
 *
 * The dashboard is the only route that composes several panels into one page,
 * so it publishes a single aggregate status (Topbar's aria-live reads it)
 * instead of one per card. A card that lost its data must not vanish
 * silently — every failed source renders a `role="alert"` block naming the
 * card that is missing.
 */

export type QueryStatusLike = {
  data: unknown;
  error: unknown;
  isPending: boolean;
  isFetching: boolean;
};

export type RegionId = "kpi" | "usage" | "leaderboard" | "pool" | "activity";

export type DashboardRegion = {
  id: RegionId;
  /** Human label used in the alert text — never invented per render. */
  label: string;
  /** Every query feeding the card has data. */
  loaded: boolean;
  /** First non-null error among the feeding queries. */
  error: unknown;
  /** Background refetch with data already on screen. */
  fetching: boolean;
};

export function errorMessage(err: unknown): string | null {
  if (err == null) return null;
  return err instanceof Error ? err.message : String(err);
}

/** One card fed by one or more queries; a partial set renders no card. */
export function aggregateRegion(
  id: RegionId,
  label: string,
  queries: readonly QueryStatusLike[],
): DashboardRegion {
  const errored = queries.find((q) => q.error != null)?.error ?? null;
  return {
    id,
    label,
    loaded: queries.length > 0 && queries.every((q) => q.data != null),
    error: errored,
    fetching: queries.some((q) => q.isFetching),
  };
}

/**
 * Cards that failed, whether or not stale data is still on screen. Mirrors
 * StatsPanel: a background-refetch failure over existing data still alerts,
 * because the numbers shown are no longer known to be current.
 */
export function failedRegions(regions: readonly DashboardRegion[]): DashboardRegion[] {
  return regions.filter((r) => r.error != null);
}

/**
 * One machine word for the page head. `error` outranks `loading` once any
 * card is known-broken, so a half-failed dashboard never reports `ready`.
 */
export function dashboardPanelStatus(regions: readonly DashboardRegion[]): {
  state: string;
  detail?: string;
} {
  const loaded = regions.filter((r) => r.loaded).length;
  const detail = loaded > 0 ? `${loaded}/${regions.length} cards` : undefined;
  if (failedRegions(regions).length > 0) return { state: "error", detail };
  // `refreshing` is only honest once something is on screen: a first load
  // that has partly resolved is still `loading`, matching StatsPanel.
  if (regions.some((r) => !r.loaded)) return { state: "loading", detail };
  if (regions.some((r) => r.fetching)) return { state: "refreshing", detail };
  return { state: "live", detail };
}
