import { useEffect } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { Phase, ProxyProgress } from "../types";

export function useProxyProgress(setPhase: React.Dispatch<React.SetStateAction<Phase>>): void {
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    listen<ProxyProgress>("proxy:progress", (event) => {
      setPhase((p) =>
        p.kind === "proxying"
          ? { kind: "proxying", progress: event.payload.progress, eta: event.payload.eta_secs }
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
