import { useEffect } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { ExportProgress, Phase } from "../types";

export function useExportProgress(setPhase: React.Dispatch<React.SetStateAction<Phase>>): void {
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    listen<ExportProgress>("export:progress", (event) => {
      setPhase((p) =>
        p.kind === "exporting"
          ? { ...p, progress: event.payload.progress }
          : p
      );
    }).then((u) => (unlisten = u));
    return () => {
      unlisten?.();
    };
    // setPhase is a setState dispatcher, stable across renders
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
}
