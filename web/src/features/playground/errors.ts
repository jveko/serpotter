export function playgroundHttpError(res: Response, data: unknown, text: string): string {
  if (typeof data === "object" && data !== null) {
    const rec = data as { title?: unknown; detail?: unknown };
    const title = rec.title != null ? String(rec.title).trim() : "";
    const detail = rec.detail != null ? String(rec.detail).trim() : "";
    if (title && detail) return `${res.status} ${title}: ${detail}`;
    if (title) return `${res.status} ${title}`;
    if (detail) return `${res.status} ${detail}`;
  }
  const fallback = (typeof data === "string" && data) || text || res.statusText || "request failed";
  return `${res.status} ${fallback}`;
}
