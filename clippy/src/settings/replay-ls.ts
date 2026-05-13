import type { CaptureMode, EncoderPref, ResolutionKind, SaveBehavior } from "./replay-types";

export const LS = {
  duration: "clippy-replay-duration-secs",
  mode: "clippy-replay-capture-mode",
  monitor: "clippy-replay-monitor-hwnd",
  saveBehavior: "clippy-replay-save-behavior",
  audioIds: "clippy-replay-audio-device-ids",
  audioNames: "clippy-replay-audio-device-names", // map: deviceId → custom name
  processLoopback: "clippy-replay-process-loopback",
  audioOpen: "clippy-replay-audio-section-open",
  gamesOpen: "clippy-replay-games-section-open",
  qualityOpen: "clippy-replay-quality-section-open",
  autoStart: "clippy-replay-auto-start",
  hideOnClose: "clippy-hide-on-close",
  diagVerbose: "clippy-diag-verbose",
  // Phase 7 quality controls. (`fps` retired — 60 is the only sane default
  // for replay buffers above 1080p; > 60 doesn't materially improve the
  // saved clip and complicates the encoder cadence model.)
  bitrate: "clippy-replay-bitrate-kbps",
  resolutionKind: "clippy-replay-resolution-kind",
  resolutionW: "clippy-replay-resolution-w",
  resolutionH: "clippy-replay-resolution-h",
  encoderPref: "clippy-replay-encoder-pref",
  keyframeSecs: "clippy-replay-keyframe-secs", // 0 = auto
  maxWorkers: "clippy-replay-max-workers",
} as const;

/** Bitrate presets (kbps). Custom = user-typed value. */
export const BITRATE_PRESETS = [
  { label: "Low", kbps: 10_000 },
  { label: "Medium", kbps: 30_000 },
  { label: "High", kbps: 50_000 },
  { label: "Ultra", kbps: 100_000 },
] as const;

export function lsNum(key: string, fallback: number): number {
  const v = localStorage.getItem(key);
  if (v == null) return fallback;
  const n = parseInt(v, 10);
  return isFinite(n) ? n : fallback;
}

export function lsBool(key: string, fallback: boolean): boolean {
  const v = localStorage.getItem(key);
  if (v == null) return fallback;
  return v === "true";
}

export function lsArr(key: string): string[] {
  try {
    const v = localStorage.getItem(key);
    if (!v) return [];
    const arr = JSON.parse(v);
    return Array.isArray(arr) ? arr.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

export function lsMap(key: string): Record<string, string> {
  try {
    const v = localStorage.getItem(key);
    if (!v) return {};
    const obj = JSON.parse(v);
    if (obj && typeof obj === "object" && !Array.isArray(obj)) {
      const out: Record<string, string> = {};
      for (const [k, val] of Object.entries(obj)) {
        if (typeof val === "string") out[k] = val;
      }
      return out;
    }
    return {};
  } catch {
    return {};
  }
}

export function getSaveBehavior(): SaveBehavior {
  return (localStorage.getItem(LS.saveBehavior) as SaveBehavior) ?? "auto-open";
}

/** Persisted "auto-start replay buffer when Clippy launches" flag. */
export function getAutoStart(): boolean {
  return localStorage.getItem(LS.autoStart) === "true";
}

/**
 * Build the argument object for `replay_start` from persisted settings so the
 * Settings UI's manual Start button and the App-mount auto-start path agree
 * on what to pass.
 */
export function getReplayStartArgs(): Record<string, unknown> {
  const durationSecs = lsNum(LS.duration, 180);
  const captureMode = (localStorage.getItem(LS.mode) as CaptureMode) ?? "perWindow";
  const monitorHwnd = localStorage.getItem(LS.monitor) ?? "";
  const audioDeviceIds = lsArr(LS.audioIds);
  const audioNamesMap = lsMap(LS.audioNames);
  // Parallel array matching audioDeviceIds order — empty string means
  // "no custom name" so the worker uses a sensible default.
  const audioDeviceNames = audioDeviceIds.map((id) => audioNamesMap[id] ?? "");
  const useProcessLoopback = lsBool(LS.processLoopback, true);
  const bitrateKbps = lsNum(LS.bitrate, 25_000);
  const encoderPreference = (localStorage.getItem(LS.encoderPref) as EncoderPref) ?? "auto";
  const keyframeSecs = lsNum(LS.keyframeSecs, 2);
  const maxConcurrentWorkers = lsNum(LS.maxWorkers, 3);

  const resKind = (localStorage.getItem(LS.resolutionKind) as ResolutionKind) ?? "source";
  let resolutionMode: unknown;
  if (resKind === "half") {
    resolutionMode = { kind: "half" };
  } else if (resKind === "custom") {
    const w = lsNum(LS.resolutionW, 1920);
    const h = lsNum(LS.resolutionH, 1080);
    resolutionMode = { kind: "custom", width: w, height: h };
  } else {
    resolutionMode = { kind: "source" };
  }

  const args: Record<string, unknown> = {
    durationSecs,
    audioDeviceIds,
    audioDeviceNames,
    useProcessLoopback,
    // fps retired from the frontend — backend defaults to 60.
    bitrateKbps,
    resolutionMode,
    encoderPreference,
    // Tauri's serde mapping for Option<Option<u32>>: pass null to disable
    // GOP override (encoder picks); a positive number sets seconds.
    keyframeIntervalSecs: keyframeSecs > 0 ? keyframeSecs : null,
    maxConcurrentWorkers,
  };
  if (captureMode === "monitor" && monitorHwnd) {
    args.captureMode = { kind: "monitor", hmonitor: monitorHwnd };
  } else {
    args.captureMode = { kind: "perWindow" };
  }
  return args;
}
