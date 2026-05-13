import type { SystemInfo } from "./replay-types";

export function shortenEncoder(name: string): string {
  // "NVIDIA H.264 Encoder MFT" → "NVENC"; "Intel® Quick Sync …" → "QSV"; etc.
  const lower = name.toLowerCase();
  if (lower.includes("nvidia")) return "NVENC";
  if (lower.includes("amd") || lower.includes("amf")) return "AMF";
  if (lower.includes("intel") || lower.includes("quick sync")) return "QSV";
  return name.length > 32 ? name.slice(0, 29) + "…" : name;
}

/**
 * Inspect the detected HW encoder list and return the vendor that Media
 * Foundation's SORTANDFILTER ordering will pick first under `Auto`. Matches
 * the priority NVENC → AMF → QSV used by `pick_encoder` in encoder.rs, so
 * the dropdown's subtitle is honest about what the worker will activate.
 */
export function resolvedAutoEncoder(sys: SystemInfo): string {
  const has = (needle: string) =>
    sys.hw_encoders.some((n) => n.toLowerCase().includes(needle));
  if (has("nvidia")) return "NVENC";
  if (has("amd") || has("amf")) return "AMF";
  if (has("intel") || has("quick sync")) return "QSV";
  return "Software";
}
