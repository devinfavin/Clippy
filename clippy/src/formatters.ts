import type { SizeLimit } from "./types";

export function fmtTime(s: number): string {
  if (!isFinite(s) || s < 0) s = 0;
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  const secStr = sec.toFixed(2).padStart(5, "0");
  if (h > 0) return `${h}:${m.toString().padStart(2, "0")}:${secStr}`;
  return `${m}:${secStr}`;
}

export function fmtEta(s: number | null): string {
  if (s == null || !isFinite(s)) return "—";
  if (s < 60) return `${Math.round(s)}s`;
  const m = Math.floor(s / 60);
  const sec = Math.round(s % 60);
  return `${m}m${sec.toString().padStart(2, "0")}s`;
}

export function fmtMb(bytes: number): string {
  const mb = bytes / (1024 * 1024);
  if (mb < 1) return `${(mb * 1024).toFixed(0)} KB`;
  if (mb < 10) return `${mb.toFixed(2)} MB`;
  return `${mb.toFixed(1)} MB`;
}

export function estimateBytesForClip(
  durationSecs: number,
  sizeLimit: SizeLimit,
  sourceBitrateBps: number | null
): number | null {
  if (sizeLimit.kind === "mb") {
    return sizeLimit.mb * 1024 * 1024;
  }
  if (sourceBitrateBps == null) return null;
  // Stream-copy estimate: source bitrate × duration / 8 + small container overhead
  return Math.round(((sourceBitrateBps * durationSecs) / 8) * 1.02);
}
