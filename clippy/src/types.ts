// Shared types and small constants used across the app.

export type VideoInfo = {
  duration_secs: number;
  width: number;
  height: number;
  fps: number;
  video_codec: string;
  audio_codec: string | null;
  container: string;
  bit_rate_bps: number | null;
};

export type ProxyResult = { play_path: string; cached: boolean; strategy: string };
export type ProxyProgress = { progress: number; elapsed_secs: number; eta_secs: number | null };
export type ExportProgress = { progress: number; elapsed_secs: number };

export type Phase =
  | { kind: "idle" }
  | { kind: "probing" }
  | { kind: "proxying"; progress: number; eta: number | null }
  | { kind: "ready" }
  | { kind: "exporting"; progress: number; current: number; total: number }
  | { kind: "error"; message: string };

export type Region = { id: string; inSecs: number; outSecs: number };

export function newRegionId(): string {
  return `r${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 7)}`;
}

export type SizeLimit =
  | { kind: "none" }
  | { kind: "mb"; mb: number; label: string };

export const SIZE_PRESETS: SizeLimit[] = [
  { kind: "none" },
  { kind: "mb", mb: 10, label: "10 MB" },
  { kind: "mb", mb: 50, label: "50 MB" },
  { kind: "mb", mb: 500, label: "500 MB" },
];

export type ExportMode = "separate" | "stitched";
