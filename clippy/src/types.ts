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
function audioTrackLabel(t: AudioTrack): string {
  if (t.title && t.title.trim().length > 0) return t.title.trim();
  return `Track ${t.index + 1}`;
}

/** Per-track color palette — 9 warm slots arranged in spectrum order
 *  (yellow → red → magenta → purple). Click the dot on a track row to
 *  pick a different slot via the ColorPicker. Used in the mixer UI and
 *  the timeline waveform overlay so a given track is always the same
 *  color everywhere. */
export const TRACK_COLORS = [
  "hsl(50, 70%, 60%)",  // gold
  "hsl(42, 65%, 60%)",  // saffron
  "hsl(28, 70%, 60%)",  // amber
  "hsl(14, 65%, 64%)",  // coral
  "hsl(355, 55%, 62%)", // crimson
  "hsl(330, 50%, 65%)", // rose
  "hsl(315, 55%, 68%)", // pink
  "hsl(295, 45%, 62%)", // magenta
  "hsl(280, 35%, 68%)", // lavender
];

function trackColor(index: number): string {
  return TRACK_COLORS[((index % TRACK_COLORS.length) + TRACK_COLORS.length) % TRACK_COLORS.length];
}

/** Per-region color palette — 9 cool slots arranged in spectrum order
 *  (green → cyan → blue → indigo). Click the dot on a region chip to pick
 *  a different slot. Used on chips, timeline bands, and the status strip
 *  so a region is the same color everywhere. */
export const REGION_COLORS = [
  "hsl(150, 40%, 50%)", // emerald
  "hsl(162, 38%, 55%)", // aquamarine
  "hsl(178, 45%, 52%)", // teal
  "hsl(188, 50%, 50%)", // sea
  "hsl(196, 50%, 58%)", // cyan
  "hsl(208, 50%, 60%)", // sky
  "hsl(218, 28%, 58%)", // slate
  "hsl(238, 35%, 62%)", // periwinkle
  "hsl(255, 30%, 65%)", // indigo
];

function regionColor(index: number): string {
  return REGION_COLORS[((index % REGION_COLORS.length) + REGION_COLORS.length) % REGION_COLORS.length];
}

/** Resolve a region's effective palette color: explicit override wins,
 *  otherwise fall back to its natural index in the regions array. */
export function resolveRegionColor(region: Region, naturalIndex: number): string {
  return regionColor(region.colorIndex ?? naturalIndex);
}

/** Per-source map of audio-track-stream-index → palette-slot. Persisted in
 *  the project state so user picks survive across reopens. Absent entry
 *  means the track uses its natural index for color. */
export type TrackColorOverrides = Record<number, number>;

/** Stride used to spread default track colors across the 9-slot palette so
 *  the common 2- or 3-track case doesn't get spectrum-adjacent neighbours.
 *  Must be coprime to TRACK_COLORS.length so every slot is reachable; with
 *  palette length 9, stride 4 gives 0→0, 1→4, 2→8, 3→3, 4→7, 5→2 etc. —
 *  3 clearly-distinct colours for the typical multi-track recording. */
const TRACK_COLOR_STRIDE = 4;

/** Default palette slot for a track when the user hasn't explicitly picked
 *  one. Exported so the ColorPicker's `selectedSlot` and the swatch dot
 *  agree on what's "currently selected" — otherwise the picker highlights
 *  one slot but the dot displays the colour of a different slot. */
export function defaultTrackColorIndex(trackIndex: number): number {
  return (trackIndex * TRACK_COLOR_STRIDE) % TRACK_COLORS.length;
}

export function resolveTrackColor(
  trackIndex: number,
  overrides: TrackColorOverrides | undefined
): string {
  const slot = overrides?.[trackIndex] ?? defaultTrackColorIndex(trackIndex);
  return trackColor(slot);
}

/** Per-source map of track-stream-index → user-renamed label. Falls back to
 *  metadata title (set by Sonar/OBS) or "Track N+1" via audioTrackLabel. */
export type TrackNameOverrides = Record<number, string>;

export function resolveTrackName(
  track: AudioTrack,
  overrides: TrackNameOverrides | undefined
): string {
  const custom = overrides?.[track.index];
  if (custom !== undefined && custom.trim().length > 0) return custom;
  return audioTrackLabel(track);
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
  // Optional override of which palette slot this region uses (0..3 for cool
  // palette). Undefined → the natural index in the regions array.
  colorIndex?: number;
};

// Per-region speed presets shown in the rail's Regions panel.
// 4× was dropped 2026-05-16: the use case (clipping from OBS recordings)
// doesn't benefit from very fast playback, and the legacy chip strip's
// space constraints no longer apply now that the Regions panel hosts the
// picker. Region.speed remains `number | undefined`, so any persisted 4×
// value still loads cleanly — just isn't offered as a fresh selection.
export const SPEED_PRESETS = [0.25, 0.5, 1, 2] as const;
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
  trackColors?: TrackColorOverrides;
  trackNames?: TrackNameOverrides;
};
