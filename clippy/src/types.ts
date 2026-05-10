// Shared types and small constants used across the app.

export type AudioTrack = {
  index: number;          // a-stream-relative index (0..N), what `0:a:N` selects
  codec: string;
  channels: number;
  layout: string | null;
  title: string | null;
  language: string | null;
};

export type VideoInfo = {
  duration_secs: number;
  width: number;
  height: number;
  fps: number;
  video_codec: string;
  audio_codec: string | null;
  audio_tracks: AudioTrack[];
  container: string;
  bit_rate_bps: number | null;
};

/** Per-track gain in the mix the user is editing. Volume is linear:
 *  0 = muted, 1 = source level, 2 = +6 dB. Stored sparsely keyed by track
 *  index so the absence of an entry means "default 1.0, unmuted". */
export type TrackMix = Record<number, { volume: number; muted: boolean }>;

/** Default human-readable name for an audio track. Uses the metadata title
 *  when present (SteelSeries/OBS often set "Game"/"Mic"/"Discord"); falls
 *  back to "Track N+1". */
export function audioTrackLabel(t: AudioTrack): string {
  if (t.title && t.title.trim().length > 0) return t.title.trim();
  return `Track ${t.index + 1}`;
}

/** Per-track color palette. Used in both the mixer UI and the timeline
 *  waveform overlay so a given track is always the same color. Picked to be
 *  visually distinct, accent-blue-friendly, and readable on a dark panel. */
export const TRACK_COLORS = [
  "#4f9dff", // blue (matches accent for track 0)
  "#5fc88a", // green
  "#ff9b3d", // orange
  "#e572d0", // magenta
  "#ffce4d", // yellow
  "#7c5cff", // violet
];

export function trackColor(index: number): string {
  return TRACK_COLORS[((index % TRACK_COLORS.length) + TRACK_COLORS.length) % TRACK_COLORS.length];
}

/** Effective gain for a track in the export sense: muted → 0, otherwise volume. */
export function trackEffectiveGain(mix: TrackMix, index: number): number {
  const e = mix[index];
  if (!e) return 1;
  if (e.muted) return 0;
  return e.volume;
}

/** Build the backend payload from a TrackMix: one entry per source track. */
export function trackMixToBackend(
  mix: TrackMix,
  totalTracks: number
): Array<{ index: number; volume: number }> {
  const out: Array<{ index: number; volume: number }> = [];
  for (let i = 0; i < totalTracks; i++) {
    out.push({ index: i, volume: trackEffectiveGain(mix, i) });
  }
  return out;
}

/** True if the mix is at defaults (every track unmuted at unity). */
export function trackMixIsDefault(mix: TrackMix, totalTracks: number): boolean {
  for (let i = 0; i < totalTracks; i++) {
    const e = mix[i];
    if (e && (e.muted || e.volume !== 1)) return false;
  }
  return true;
}

/** True if two mixes resolve to the same effective gain for every track. */
export function mixesEqual(
  a: TrackMix | undefined,
  b: TrackMix | undefined,
  totalTracks: number
): boolean {
  const aa = a ?? {};
  const bb = b ?? {};
  for (let i = 0; i < totalTracks; i++) {
    if (trackEffectiveGain(aa, i) !== trackEffectiveGain(bb, i)) return false;
  }
  return true;
}

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

// Spatial crop in source-pixel coordinates. x/y is the top-left origin.
// Persisted per region; `undefined` means no crop (stream-copy stays valid).
export type Crop = { x: number; y: number; w: number; h: number };

export type Region = {
  id: string;
  inSecs: number;
  outSecs: number;
  crop?: Crop;
  // Playback speed multiplier (0.25..4). Undefined / 1 = no effect.
  speed?: number;
  // Optional per-region audio mix override. Undefined → falls back to the
  // source-default mix for both live preview and export.
  mix?: TrackMix;
};

// Per-region speed presets shown in the chip popover.
export const SPEED_PRESETS = [0.25, 0.5, 1, 2, 4] as const;
export type SpeedPreset = (typeof SPEED_PRESETS)[number];

export function speedLabel(s: number | undefined): string {
  const v = s ?? 1;
  if (Number.isInteger(v)) return `${v}×`;
  return `${v}×`;
}

// Aspect ratio presets for the crop overlay. "free" = no constraint;
// "source" = match the source aspect; numeric = w/h ratio.
export type AspectLock =
  | { kind: "free" }
  | { kind: "source" }
  | { kind: "ratio"; w: number; h: number; label: string };

export const ASPECT_PRESETS: AspectLock[] = [
  { kind: "free" },
  { kind: "ratio", w: 16, h: 9, label: "16:9" },
  { kind: "ratio", w: 9, h: 16, label: "9:16" },
  { kind: "ratio", w: 1, h: 1, label: "1:1" },
  { kind: "ratio", w: 4, h: 3, label: "4:3" },
  { kind: "source" },
];

/** Same crop dims (or both undefined). Used to gate stitched export. */
export function cropsEqual(a: Crop | undefined, b: Crop | undefined): boolean {
  if (!a && !b) return true;
  if (!a || !b) return false;
  return a.x === b.x && a.y === b.y && a.w === b.w && a.h === b.h;
}

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

// Container/codec choice. MP3 = audio-only; GIF = silent looping animation.
export type ExportFormat = "mp4" | "mp3" | "gif";

// Fixed bitrate used by the MP3 path. Mirrors backend MP3_BITRATE.
export const MP3_BITRATE_BPS = 192_000;

// GIF cap (matches backend GIF_FPS).
export const GIF_FPS = 15;

// GIF target-width presets. "source" means cap to source width (no upscale
// either way — backend uses min(target, iw)).
export type GifResolution =
  | { kind: "px"; w: number; label: string }
  | { kind: "source" };
export const GIF_RESOLUTION_PRESETS: GifResolution[] = [
  { kind: "px", w: 480,  label: "Small" },
  { kind: "px", w: 960,  label: "Medium" },
  { kind: "px", w: 1280, label: "Large" },
  { kind: "source" },
];
export const GIF_DEFAULT_RESOLUTION: GifResolution = GIF_RESOLUTION_PRESETS[2]; // Large (1280)

/** Loose bytes/sec estimate for the chosen GIF width at 15fps. Pure heuristic;
 *  GIF size is dominated by motion + palette repetition. ~0.35 bytes/pixel/frame
 *  is a workable middle for gameplay. */
export function gifBytesPerSec(targetWidth: number, sourceAspect: number): number {
  const w = targetWidth;
  const h = Math.round(w / Math.max(0.1, sourceAspect));
  const pixelsPerFrame = w * h;
  return Math.round(pixelsPerFrame * GIF_FPS * 0.35);
}

// Persisted-per-source state. Lives in a sidecar JSON next to the proxy cache.
// Add fields here and they'll round-trip on file reopen automatically.
export type ProjectState = {
  version: 1;
  regions: Region[];
  trackMix?: TrackMix;
};
