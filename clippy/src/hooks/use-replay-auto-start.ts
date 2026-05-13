import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getAutoStart, getReplayStartArgs } from "../settings/replay-ls";

/// Auto-start the replay buffer at app launch if the user opted in. Runs
/// once after the component mounts; failures are logged but never block
/// the rest of startup.
export function useReplayAutoStart(): void {
  useEffect(() => {
    if (!getAutoStart()) return;
    const t = setTimeout(() => {
      invoke("replay_start", getReplayStartArgs()).catch((e) => {
        console.warn("[clippy] replay auto-start failed:", e);
      });
    }, 400);
    return () => clearTimeout(t);
  }, []);
}
