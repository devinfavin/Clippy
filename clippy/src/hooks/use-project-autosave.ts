import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  ProjectState,
  Region,
  TrackColorOverrides,
  TrackMix,
  TrackNameOverrides,
} from "../types";

/// Debounced save of the project state (regions, crops, speeds, track mix,
/// track colors, track names) to a sidecar JSON next to the proxy cache.
export function useProjectAutosave(args: {
  srcPath: string | null;
  regions: Region[];
  trackMix: TrackMix;
  trackColors: TrackColorOverrides;
  trackNames: TrackNameOverrides;
}): void {
  const { srcPath, regions, trackMix, trackColors, trackNames } = args;
  useEffect(() => {
    if (!srcPath) return;
    const handle = window.setTimeout(() => {
      const state: ProjectState = { version: 1, regions, trackMix, trackColors, trackNames };
      invoke("save_project", { srcPath, state }).catch((err) =>
        console.warn("[clippy] save_project failed:", err)
      );
    }, 600);
    return () => window.clearTimeout(handle);
  }, [srcPath, regions, trackMix, trackColors, trackNames]);
}
