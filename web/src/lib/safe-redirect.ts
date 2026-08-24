import { SECTIONS, type SectionId } from "./constants";

const ALLOWED = new Set<string>(["/stats", ...SECTIONS.map((s) => `/${s.id}`)]);

/** Path-only post-login target. Never accepts absolute URLs. */
export function safeRedirectPath(value: unknown): string {
  if (typeof value !== "string") return "/dashboard";
  const raw = value.trim();
  if (!raw.startsWith("/") || raw.startsWith("//")) return "/dashboard";
  const noQuery = raw.split("?")[0]?.split("#")[0] ?? "/";
  if (noQuery === "/" || noQuery === "") return "/dashboard";
  if (ALLOWED.has(noQuery)) return noQuery;
  // single segment /stats style
  const m = /^\/([a-z0-9-]+)$/.exec(noQuery);
  if (m && SECTIONS.some((s) => s.id === m[1])) return `/${m[1] as SectionId}`;
  return "/dashboard";
}
